use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use ring::rand::{SecureRandom, SystemRandom};
use tokio::{
    sync::watch,
    time::{Instant, timeout},
};
use tokio_tungstenite::tungstenite::Message;
use x25519_dalek::StaticSecret;

use crate::{
    ServiceError, ServiceState,
    api::Connector,
    control,
    noise::{Initiator, Session},
    pairing::Pairing,
    wire::{Assembler, Body, Frame, Packetizer, Request, StreamMux, decode_response, encode_request},
};

const START_TIMEOUT: Duration = Duration::from_secs(15);
type State = watch::Sender<(ServiceState, Option<ServiceError>)>;

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
    pairing: &Pairing,
    connector: &C,
    random: &impl Random,
    state: &State,
) -> Result<(), ServiceError> {
    let mut link = timeout(START_TIMEOUT, establish(pairing, connector, random))
        .await
        .map_err(|_| ServiceError::PeerUnavailable)??;
    state.send_replace((ServiceState::Registered, None));
    loop {
        link.receive_control().await?;
    }
}

async fn establish<C: Connector>(
    pairing: &Pairing,
    connector: &C,
    random: &impl Random,
) -> Result<ControlLink<C::Socket>, ServiceError> {
    let lease = connector.lookup(pairing).await?;
    let mut socket = connector.upgrade(pairing, &lease).await?;
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
    let mut link = ControlLink::open(socket, noise, pairing.node_id.expose()).await?;
    loop {
        match link.receive_control().await? {
            Some(true) => return Ok(link),
            Some(false) => return Err(ServiceError::NotReady),
            None => {}
        }
    }
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

use futures_util::{Sink, Stream};
use tokio_tungstenite::tungstenite::Error;

impl<S> ControlLink<S>
where
    S: Sink<Message, Error = Error> + Stream<Item = Result<Message, Error>> + Unpin,
{
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
        let data = control::encode(&control::registration(node_id)).map_err(|_| ServiceError::UnsafePairing)?;
        link.send(&Frame {
            id: link.control_id,
            body: Body::Chunk { data, end_body: false },
        })
        .await?;
        Ok(link)
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
            self.socket
                .send(Message::binary(packet))
                .await
                .map_err(|_| ServiceError::PeerUnavailable)?;
        }
        Ok(())
    }

    async fn receive_control(&mut self) -> Result<Option<bool>, ServiceError> {
        let packet = recv_binary(&mut self.socket).await?;
        let chunk = self
            .noise
            .receive
            .decrypt(packet, &[])
            .map_err(|_| ServiceError::PeerUnavailable)?;
        let Some(bytes) = self
            .assembler
            .push(&chunk, Instant::now())
            .map_err(|_| ServiceError::PeerUnavailable)?
        else {
            return Ok(None);
        };
        let frame = decode_response(&bytes).map_err(|_| ServiceError::PeerUnavailable)?;
        let Some((id, body)) = self.mux.accept(frame) else {
            return Ok(None);
        };
        if id != self.control_id {
            return Ok(None);
        }
        let body = body.map_err(|_| ServiceError::NotReady)?;
        let data = match body {
            Body::Response(response) if (200..300).contains(&response.status) && !response.end_body => response.body,
            Body::Chunk { data, end_body: false } => data,
            _ => return Err(ServiceError::NotReady),
        };
        let mut data = data.as_slice();
        let mut ack = None;
        while let Some(value) = self
            .decoder
            .next(&mut data)
            .map_err(|_| ServiceError::PeerUnavailable)?
        {
            ack = ack.or_else(|| control::register_ack(&value));
        }
        Ok(ack)
    }
}

async fn recv_binary<S>(socket: &mut S) -> Result<Vec<u8>, ServiceError>
where
    S: Sink<Message, Error = Error> + Stream<Item = Result<Message, Error>> + Unpin,
{
    loop {
        match socket.next().await {
            Some(Ok(Message::Binary(bytes))) => return Ok(bytes.to_vec()),
            Some(Ok(Message::Ping(_))) => socket.flush().await.map_err(|_| ServiceError::PeerUnavailable)?,
            Some(Ok(Message::Pong(_))) => {}
            _ => return Err(ServiceError::PeerUnavailable),
        }
    }
}
