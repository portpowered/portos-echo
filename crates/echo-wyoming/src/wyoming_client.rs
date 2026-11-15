use crate::{
    ClientOptions, Error, EventStream, PlayOutcome,
    wyoming_session::{self, Command},
};
use echo_contracts::wyoming::{AudioFormat, WyomingEvent};
use std::{net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::timeout,
};

/// Owns one connection and its reader. Drop aborts both tasks.
pub struct WyomingClient {
    commands: mpsc::Sender<Command>,
    events: Option<EventStream>,
    terminal: watch::Receiver<Option<Error>>,
    task: JoinHandle<()>,
    deadline: Duration,
}

impl WyomingClient {
    /// Connect to a Wyoming endpoint without registering the satellite yet.
    /// # Errors
    /// Invalid options, connect timeout or refused connection.
    pub async fn connect(addr: SocketAddr, opts: ClientOptions) -> Result<Self, Error> {
        opts.validate()?;
        let socket = timeout(opts.connect_timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(|_| Error::Disconnected)?;
        Ok(Self::from_io(socket, opts))
    }

    pub(crate) fn from_io<T: AsyncRead + AsyncWrite + Unpin + Send + 'static>(io: T, opts: ClientOptions) -> Self {
        let (commands, receiver) = mpsc::channel(opts.command_capacity);
        let (events, event_receiver) = mpsc::channel(opts.event_capacity);
        let (end, terminal) = watch::channel(None);
        let deadline = opts.operation_timeout;
        let task = tokio::spawn(wyoming_session::serve(io, opts, receiver, events, end));
        Self {
            commands,
            events: Some(EventStream {
                receiver: event_receiver,
                terminal: terminal.clone(),
            }),
            terminal,
            task,
            deadline,
        }
    }

    async fn request<T>(&self, command: Command, reply: oneshot::Receiver<Result<T, Error>>) -> Result<T, Error> {
        let work = async {
            self.commands.send(command).await.map_err(|_| self.failure())?;
            reply.await.map_err(|_| self.failure())?
        };
        if let Ok(result) = timeout(self.deadline, work).await {
            result
        } else {
            self.task.abort();
            Err(Error::Timeout)
        }
    }
    fn failure(&self) -> Error {
        self.terminal.borrow().unwrap_or(Error::Closed)
    }

    /// Request the satellite's advertised info.
    /// # Errors
    /// Busy, connection failure or operation timeout.
    pub async fn describe(&self) -> Result<WyomingEvent, Error> {
        let (tx, rx) = oneshot::channel();
        self.request(Command::Describe(tx), rx).await
    }

    /// Enable microphone pipelines; repeated calls are idempotent.
    /// # Errors
    /// Connection failure or operation timeout.
    pub async fn run_satellite(&self) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();
        self.request(Command::Register(tx), rx).await
    }

    /// Take the sole event receiver.
    /// # Errors
    /// Busy if the receiver has already been taken.
    pub fn events(&mut self) -> Result<EventStream, Error> {
        self.events.take().ok_or(Error::Busy)
    }

    /// Send a reply, waiting for `played` even for an empty source.
    /// The source must be bounded; callers retain ownership of unsent input.
    /// # Errors
    /// Busy, oversized frame, connection failure or operation timeout.
    pub async fn play(&self, format: AudioFormat, frames: mpsc::Receiver<Vec<u8>>) -> Result<PlayOutcome, Error> {
        let (tx, rx) = oneshot::channel();
        self.request(Command::Play(format, frames, tx), rx).await
    }

    /// Close and reap this connection's tasks.
    /// # Errors
    /// Returns Closed if the connection task already stopped.
    pub async fn close(&self) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();
        self.request(Command::Close(tx), rx).await
    }
}

impl Drop for WyomingClient {
    fn drop(&mut self) {
        self.task.abort();
    }
}
