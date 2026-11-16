use crate::wyoming_events;
use crate::{
    ClientOptions, CutStats, Error, EventStream, PlayOutcome,
    wyoming_session::{self, Command},
};
use echo_contracts::wyoming::{AudioFormat, WyomingEvent};
use std::{future::Future, net::SocketAddr, time::Duration};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::timeout,
};

/// Owns the reconnect supervisor. Drop aborts its current connection and reader.
pub struct WyomingClient {
    pub(crate) neutral_barrier: bool,
    commands: mpsc::Sender<Command>,
    events: Option<EventStream>,
    terminal: watch::Receiver<Option<Error>>,
    task: JoinHandle<()>,
    deadline: Duration,
    reset: Option<watch::Sender<u64>>,
}

impl WyomingClient {
    /// Connect without registering yet. Cancelling this future stops initial retries.
    /// # Errors
    /// Invalid options. Refusal/timeouts retry with capped backoff until cancelled.
    pub async fn connect(addr: SocketAddr, opts: ClientOptions) -> Result<Self, Error> {
        opts.validate()?;
        let mut connector = move || tokio::net::TcpStream::connect(addr);
        let socket = wyoming_session::wyoming_reconnect::connect(&opts, &mut connector, false).await;
        Ok(Self::reconnecting(socket, opts, connector))
    }

    pub(crate) fn reconnecting<T, F, Fut>(socket: T, opts: ClientOptions, connector: F) -> Self
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        F: FnMut() -> Fut + Send + 'static,
        Fut: Future<Output = std::io::Result<T>> + Send + 'static,
    {
        let (commands, receiver) = mpsc::channel(opts.command_capacity);
        let (events, event_receiver) = wyoming_events::channel(opts.event_capacity);
        let (end, terminal) = watch::channel(None);
        let (reset, resets) = watch::channel(0_u64);
        let deadline = opts.operation_timeout;
        let supervisor = wyoming_session::wyoming_reconnect::Supervisor {
            opts,
            commands: receiver,
            events,
            end,
            reset: resets,
        };
        let task = tokio::spawn(supervisor.serve(socket, connector));
        Self {
            neutral_barrier: false,
            commands,
            events: Some(EventStream {
                receiver: event_receiver,
                terminal: terminal.clone(),
            }),
            terminal,
            task,
            deadline,
            reset: Some(reset),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_io<T: AsyncRead + AsyncWrite + Unpin + Send + 'static>(io: T, opts: ClientOptions) -> Self {
        let (commands, receiver) = mpsc::channel(opts.command_capacity);
        let (events, event_receiver) = wyoming_events::channel(opts.event_capacity);
        let (end, terminal) = watch::channel(None);
        let deadline = opts.operation_timeout;
        let task = tokio::spawn(wyoming_session::serve(io, opts, receiver, events, end));
        Self {
            neutral_barrier: false,
            commands,
            events: Some(EventStream {
                receiver: event_receiver,
                terminal: terminal.clone(),
            }),
            terminal,
            task,
            deadline,
            reset: None,
        }
    }

    async fn request<T>(&self, command: Command, reply: oneshot::Receiver<Result<T, Error>>) -> Result<T, Error> {
        self.request_timed(command, reply, self.deadline).await
    }

    async fn request_timed<T>(
        &self,
        command: Command,
        reply: oneshot::Receiver<Result<T, Error>>,
        deadline: Duration,
    ) -> Result<T, Error> {
        let work = async {
            self.commands.send(command).await.map_err(|_| self.failure())?;
            reply.await.map_err(|_| self.failure())?
        };
        if let Ok(result) = timeout(deadline, work).await {
            result
        } else {
            if let Some(reset) = &self.reset {
                reset.send_modify(|epoch| *epoch = epoch.wrapping_add(1));
            } else {
                self.task.abort();
            }
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
    /// The source must be bounded. Internal reply overflow evicts oldest unsent
    /// frames. Cancelling this future closes the connection before another reply.
    /// # Errors
    /// Busy, oversized frame, connection failure or operation timeout.
    pub async fn play(&self, format: AudioFormat, frames: mpsc::Receiver<Vec<u8>>) -> Result<PlayOutcome, Error> {
        self.play_with_timeout(format, frames, self.deadline).await
    }

    /// Send a bounded reply with its own lifetime, including source production and `played`.
    /// Ordinary control requests retain the configured operation timeout.
    /// Cancellation and timeout invalidate playback exactly as for `play`.
    /// # Errors
    /// The same failures as `play`; a zero timeout is invalid.
    pub async fn play_with_timeout(
        &self,
        format: AudioFormat,
        frames: mpsc::Receiver<Vec<u8>>,
        deadline: Duration,
    ) -> Result<PlayOutcome, Error> {
        if deadline.is_zero() {
            return Err(Error::InvalidOptions);
        }
        let (tx, rx) = oneshot::channel();
        self.request_timed(Command::Play(format, frames, tx), rx, deadline)
            .await
    }

    /// Discard unsent reply audio and send one early stop. Playback still waits
    /// for `played`; audio already sent to hassmic drains normally.
    /// # Errors
    /// Connection failure or operation timeout.
    pub async fn cut(&self) -> Result<(), Error> {
        self.cut_with_stats().await.map(|_| ())
    }

    /// Discard unsent reply frames and report their exact retained count.
    /// Already written frames drain normally; `play` still waits for `played`.
    /// Repeated cuts and cuts without an active reply report zero.
    /// # Errors
    /// Connection failure or operation timeout.
    pub async fn cut_with_stats(&self) -> Result<CutStats, Error> {
        let (tx, rx) = oneshot::channel();
        self.request(Command::CutWithStats(tx), rx).await
    }

    /// Pause microphone capture and end the local run. Repeated calls are
    /// idempotent; `run_satellite` explicitly resumes capture.
    /// # Errors
    /// Connection failure or operation timeout.
    pub async fn pause(&self) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();
        self.request(Command::Pause(tx), rx).await
    }

    /// Send one device-visible error through the session writer, keeping the connection.
    /// # Errors
    /// Connection failure, capacity limit, or operation timeout.
    pub async fn send_error(&self, code: &str, text: &str) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();
        self.request(Command::Error(code.into(), text.into(), tx), rx).await
    }

    /// Close and reap this connection's tasks.
    /// # Errors
    /// Returns Closed if the connection task already stopped.
    pub async fn close(&self) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();
        self.request(Command::Close(tx), rx).await?;
        let mut terminal = self.terminal.clone();
        timeout(self.deadline, async {
            while terminal.borrow_and_update().is_none() {
                if terminal.changed().await.is_err() {
                    break;
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout)
    }
}

impl Drop for WyomingClient {
    fn drop(&mut self) {
        self.task.abort();
    }
}
