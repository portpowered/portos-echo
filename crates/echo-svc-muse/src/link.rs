mod chats;

use std::time::Duration;

use futures_util::{Sink, SinkExt, Stream, StreamExt};
use ring::rand::{SecureRandom, SystemRandom};
use tokio::{
    sync::{mpsc, watch},
    time::{Instant, sleep, timeout},
};
use tokio_tungstenite::tungstenite::{Error, Message};
use x25519_dalek::StaticSecret;

use crate::{
    ServiceError, ServiceState,
    api::{Connector, RefreshError, UpgradeError},
    chat::Command,
    control,
    link::chats::Chats,
    noise::{Initiator, Session},
    pairing::Pairing,
    tasks::Tasks,
    wire::{Assembler, Body, Frame, InvalidWire, Packetizer, Request, StreamMux, decode_response, encode_request},
};

const START_TIMEOUT: Duration = Duration::from_secs(15);
const FIRST_RETRY: Duration = Duration::from_secs(2);
const MAX_RETRY: Duration = Duration::from_secs(60);
const ROTATE_AFTER: u64 = 3 * 60 * 60;
type State = watch::Sender<(ServiceState, Option<ServiceError>)>;
pub(super) type Endpoint = watch::Sender<Option<mpsc::Sender<Command>>>;

pub(super) trait Random: Send + Sync + 'static {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), ServiceError>;
}

pub(super) struct OsRandom(SystemRandom);

impl OsRandom {
    pub(super) fn new() -> Self {
        Self(SystemRandom::new())
    }
}

impl Random for OsRandom {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), ServiceError> {
        self.0.fill(bytes).map_err(|_| ServiceError::PeerUnavailable)
    }
}

pub(super) async fn run<C: Connector>(
    pairing: &mut Pairing,
    connector: &C,
    tasks: &Tasks,
    handler: &control::Handler,
    state: &State,
    endpoint: &Endpoint,
    mut stop: watch::Receiver<bool>,
) -> Result<ServiceState, ServiceError> {
    let mut delay = FIRST_RETRY;
    let clock = &*tasks.clock;
    loop {
        if *stop.borrow() || stop.has_changed().is_err() {
            return Ok(ServiceState::Stopped);
        }
        endpoint.send_replace(None);
        state.send_replace((ServiceState::Connecting, None));
        let mut rotates_at = pairing.saved_at().saturating_add(ROTATE_AFTER);
        if clock.now() >= rotates_at {
            if rotate(pairing, connector, clock).await? == Rotation::Removed {
                return Ok(ServiceState::PairingRemoved);
            }
            if *stop.borrow() || stop.has_changed().is_err() {
                return Ok(ServiceState::Stopped);
            }
            rotates_at = pairing.saved_at().saturating_add(ROTATE_AFTER);
        }
        let outcome = tokio::select! {
            biased;
            _ = stop.changed() => return Ok(ServiceState::Stopped),
            () = clock.wait_until(rotates_at) => continue,
            outcome = run_attempt(pairing, connector, tasks, handler, state, endpoint) => outcome,
        };
        endpoint.send_replace(None);
        let Err(error) = outcome else {
            return Ok(ServiceState::PairingRemoved);
        };
        if state.borrow().0 == ServiceState::Registered {
            delay = FIRST_RETRY;
        }
        state.send_replace((ServiceState::Disconnected, Some(error)));
        tokio::select! {
            biased;
            _ = stop.changed() => return Ok(ServiceState::Stopped),
            () = clock.wait_until(rotates_at) => {}
            () = sleep(delay) => delay = (delay * 2).min(MAX_RETRY),
        }
    }
}

#[derive(PartialEq, Eq)]
enum Rotation {
    Replaced,
    Removed,
}

async fn rotate(
    pairing: &mut Pairing,
    connector: &impl Connector,
    clock: &dyn crate::clock::Clock,
) -> Result<Rotation, ServiceError> {
    // Once the request is sent, finish handling its response before honoring stop.
    match connector.refresh(pairing).await {
        Ok(tokens) => {
            pairing.replace_tokens(tokens, clock.now())?;
            Ok(Rotation::Replaced)
        }
        Err(RefreshError::Revoked) => Ok(Rotation::Removed),
        Err(RefreshError::Unavailable) => Err(ServiceError::PeerUnavailable),
    }
}

async fn run_attempt<C: Connector>(
    pairing: &Pairing,
    connector: &C,
    tasks: &Tasks,
    handler: &control::Handler,
    state: &State,
    endpoint: &Endpoint,
) -> Result<(), ServiceError> {
    let (mut link, registration) = timeout(START_TIMEOUT, establish(pairing, connector, &*tasks.random))
        .await
        .map_err(|_| ServiceError::PeerUnavailable)??;
    let Registration::Accepted(pending) = registration else {
        return Ok(());
    };
    let (commands, receiver) = mpsc::channel(16);
    endpoint.send_replace(Some(commands));
    state.send_replace((ServiceState::Registered, None));
    link.registered(tasks, pairing, handler, receiver, pending).await
}

async fn connect<C: Connector>(pairing: &Pairing, connector: &C) -> Result<C::Socket, ServiceError> {
    let lease = connector.lookup(pairing).await?;
    match connector.upgrade(pairing, &lease).await {
        Ok(socket) => Ok(socket),
        Err(UpgradeError::Unavailable) => Err(ServiceError::PeerUnavailable),
        Err(UpgradeError::LeaseRejected) => {
            let fresh = connector.lookup(pairing).await?;
            connector
                .upgrade(pairing, &fresh)
                .await
                .map_err(|_| ServiceError::PeerUnavailable)
        }
    }
}

async fn establish<C: Connector>(
    pairing: &Pairing,
    connector: &C,
    random: &dyn Random,
) -> Result<(ControlLink<C::Socket>, Registration), ServiceError> {
    let mut socket = connect(pairing, connector).await?;
    let mut bytes = [0; 32];
    random.fill(&mut bytes)?;
    let (handshake, first) = Initiator::start(pairing.static_key.expose(), StaticSecret::from(bytes), &[], &[])
        .map_err(|_| ServiceError::PeerUnavailable)?;
    socket
        .send(Message::binary(first))
        .await
        .map_err(|_| ServiceError::PeerUnavailable)?;
    let second = recv_binary(&mut socket).await?;
    let (payload, third, noise) = handshake
        .finish(&second, &[])
        .map_err(|_| ServiceError::PeerUnavailable)?;
    if !payload.is_empty() {
        return Err(ServiceError::PeerUnavailable);
    }
    socket
        .send(Message::binary(third))
        .await
        .map_err(|_| ServiceError::PeerUnavailable)?;
    let mut link = ControlLink::open(socket, noise, pairing.node_id().expose()).await?;
    let pending = link.register().await?;
    Ok((link, pending))
}

struct ControlLink<S> {
    socket: S,
    noise: Session,
    mux: StreamMux,
    control_id: i64,
    decoder: control::Decoder,
    packetizer: Packetizer,
    assembler: Assembler,
}

enum Registration {
    Accepted(Vec<serde_json::Value>),
    Removed,
}

trait Socket: Sink<Message, Error = Error> + Stream<Item = Result<Message, Error>> + Unpin {}
impl<S> Socket for S where S: Sink<Message, Error = Error> + Stream<Item = Result<Message, Error>> + Unpin {}

impl<S: Socket> ControlLink<S> {
    async fn open(socket: S, noise: Session, node_id: &str) -> Result<Self, ServiceError> {
        let mut mux = StreamMux::default();
        let open = mux
            .open(Request {
                verb: "POST".into(),
                path: "/link-control".into(),
                ..Request::default()
            })
            .map_err(|_| ServiceError::PeerUnavailable)?;
        let mut link = Self {
            socket,
            noise,
            mux,
            control_id: open.id,
            decoder: control::Decoder::default(),
            packetizer: Packetizer::default(),
            assembler: Assembler::default(),
        };
        link.send(&open).await?;
        link.send_control(&control::registration(node_id)).await?;
        Ok(link)
    }

    async fn register(&mut self) -> Result<Registration, ServiceError> {
        loop {
            let (_, body) = self.receive().await?;
            // Removal applies even when the VM has not acknowledged registration yet.
            let mut messages = self.decode_control(body)?.into_iter();
            for message in &mut messages {
                if control::unpaired(&message) {
                    return Ok(Registration::Removed);
                }
                match control::register_ack(&message) {
                    Some(true) => return Ok(Registration::Accepted(messages.collect())),
                    Some(false) => return Err(ServiceError::NotReady),
                    None => {}
                }
            }
        }
    }

    async fn registered(
        &mut self,
        tasks: &Tasks,
        pairing: &Pairing,
        handler: &control::Handler,
        mut commands: mpsc::Receiver<Command>,
        pending: Vec<serde_json::Value>,
    ) -> Result<(), ServiceError> {
        let mut chats = Chats::default();
        let mut invokes = control::Invocations::new(handler, tasks);
        if self.handle_control(&mut invokes, pending).await? {
            return Ok(());
        }
        loop {
            tokio::select! {
                frame = self.receive() => {
                    let (id, body) = frame?;
                    if id == self.control_id {
                        let messages = self.decode_control(body)?;
                        if self.handle_control(&mut invokes, messages).await? {
                            return Ok(());
                        }
                    } else {
                        chats.receive(self, tasks, id, body).await?;
                    }
                }
                Some(command) = commands.recv() => chats.command(self, tasks, pairing.node_id(), command).await?,
                () = Chats::wait(tasks, chats.deadline()) => chats.expire(self, tasks).await?,
                reply = invokes.next() => {
                    let data = control::encode_result(reply.message).map_err(|_| ServiceError::PeerUnavailable)?;
                    self.send_control_data(data).await?;
                    if let Some(task) = reply.task {
                        chats.end_task(self, &task).await?;
                    }
                }
            }
        }
    }

    async fn handle_control(
        &mut self,
        invokes: &mut control::Invocations<'_>,
        messages: Vec<serde_json::Value>,
    ) -> Result<bool, ServiceError> {
        if messages.iter().any(control::unpaired) {
            return Ok(true);
        }
        for message in messages {
            if let Some(reply) = invokes.admit(message) {
                self.send_control(&reply.message).await?;
            }
        }
        Ok(false)
    }

    async fn send_control(&mut self, value: &serde_json::Value) -> Result<(), ServiceError> {
        let data = control::encode(value).map_err(|_| ServiceError::PeerUnavailable)?;
        self.send_control_data(data).await
    }

    async fn send_control_data(&mut self, data: Vec<u8>) -> Result<(), ServiceError> {
        self.send(&Frame {
            id: self.control_id,
            body: Body::Chunk { data, end_body: false },
        })
        .await
    }

    async fn open_request(&mut self, mut request: Request) -> Result<i64, ServiceError> {
        let mut upload = Vec::new();
        if request.body.len() > 64 * 1024 {
            upload = std::mem::take(&mut request.body);
            request.end_body = false;
        }
        let frame = self.mux.open(request).map_err(|_| ServiceError::PeerUnavailable)?;
        self.send(&frame).await?;
        let mut chunks = upload.chunks(16 * 1024).peekable();
        while let Some(chunk) = chunks.next() {
            let body = Body::Chunk {
                data: chunk.to_vec(),
                end_body: chunks.peek().is_none(),
            };
            self.send(&Frame { id: frame.id, body }).await?;
        }
        Ok(frame.id)
    }

    async fn reset(&mut self, id: i64) -> Result<(), ServiceError> {
        if self.mux.forget(id) {
            self.send(&Frame {
                id,
                body: Body::Reset {
                    code: 1,
                    reason: "request ended".into(),
                },
            })
            .await?;
        }
        Ok(())
    }

    async fn send_advisory(&mut self, request: Request) -> Result<(), ServiceError> {
        let stream = self.open_request(request).await?;
        // Cancellation is already committed. The advisory requires no response
        // and the peer may finish handling it without waiting on another frame.
        self.mux.forget(stream);
        Ok(())
    }

    async fn send(&mut self, frame: &Frame) -> Result<(), ServiceError> {
        for part in self
            .packetizer
            .split(&encode_request(frame))
            .map_err(|_| ServiceError::PeerUnavailable)?
        {
            let packet = self
                .noise
                .send
                .encrypt(&part, &[])
                .map_err(|_| ServiceError::PeerUnavailable)?;
            timeout(START_TIMEOUT, self.socket.send(Message::binary(packet)))
                .await
                .map_err(|_| ServiceError::PeerUnavailable)?
                .map_err(|_| ServiceError::PeerUnavailable)?;
        }
        Ok(())
    }

    async fn receive(&mut self) -> Result<(i64, Result<Body, InvalidWire>), ServiceError> {
        loop {
            let packet = recv_binary(&mut self.socket).await?;
            let chunk = self
                .noise
                .receive
                .decrypt(packet, &[])
                .map_err(|_| ServiceError::PeerUnavailable)?;
            if let Some(bytes) = self
                .assembler
                .push(&chunk, Instant::now())
                .map_err(|_| ServiceError::PeerUnavailable)?
            {
                let frame = decode_response(&bytes).map_err(|_| ServiceError::PeerUnavailable)?;
                if let Some(incoming) = self.mux.accept(frame) {
                    return Ok(incoming);
                }
            }
        }
    }

    fn decode_control(&mut self, body: Result<Body, InvalidWire>) -> Result<Vec<serde_json::Value>, ServiceError> {
        let body = body.map_err(|_| ServiceError::NotReady)?;
        let data = match body {
            Body::Response(response) if (200..300).contains(&response.status) && !response.end_body => response.body,
            Body::Chunk { data, end_body: false } => data,
            _ => return Err(ServiceError::NotReady),
        };
        let mut data = data.as_slice();
        let mut messages = Vec::new();
        while let Some(value) = self
            .decoder
            .next(&mut data)
            .map_err(|_| ServiceError::PeerUnavailable)?
        {
            messages.push(value);
        }
        Ok(messages)
    }
}

async fn recv_binary(socket: &mut impl Socket) -> Result<Vec<u8>, ServiceError> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Binary(bytes))) => return Ok(bytes.to_vec()),
            Some(Ok(Message::Ping(_))) => socket.flush().await.map_err(|_| ServiceError::PeerUnavailable)?,
            Some(Ok(Message::Pong(_))) => {}
            _ => return Err(ServiceError::PeerUnavailable),
        }
    }
}
