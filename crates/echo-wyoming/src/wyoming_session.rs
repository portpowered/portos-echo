use crate::{ClientOptions, Error, PlayOutcome, ReceivedEvent, wyoming_route::Router, wyoming_wire};
use echo_contracts::wyoming::{AudioChunkData, AudioFormat, WyomingEvent};
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncWrite, BufReader},
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::timeout,
};

type Reply<T> = oneshot::Sender<Result<T, Error>>;
pub(crate) enum Command {
    Describe(Reply<WyomingEvent>),
    Register(Reply<()>),
    Play(AudioFormat, mpsc::Receiver<Vec<u8>>, Reply<PlayOutcome>),
    Close(Reply<()>),
}

struct Playback {
    format: AudioFormat,
    frames: mpsc::Receiver<Vec<u8>>,
    reply: Reply<PlayOutcome>,
    stopped: bool,
}

struct Session<W> {
    writer: W,
    opts: ClientOptions,
    router: Router,
    describe: Option<Reply<WyomingEvent>>,
    play: Option<Playback>,
    registered: bool,
}

struct ReaderTask(JoinHandle<()>);
impl Drop for ReaderTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) async fn serve<T: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    io: T,
    opts: ClientOptions,
    mut commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<ReceivedEvent>,
    end: watch::Sender<Option<Error>>,
) {
    let (reader, writer) = tokio::io::split(io);
    let (tx, mut incoming) = mpsc::channel(1);
    let limit = opts.max_frame_bytes;
    let mut reader_task = ReaderTask(tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        loop {
            let frame = wyoming_wire::read(&mut reader, limit).await;
            let failed = frame.is_err();
            if tx.send(frame).await.is_err() || failed {
                break;
            }
        }
    }));
    let mut session = Session {
        writer,
        router: Router {
            current: None,
            events,
            capacity: opts.mic_capacity,
        },
        opts,
        describe: None,
        play: None,
        registered: false,
    };
    let result = session.run(&mut commands, &mut incoming).await;
    session
        .router
        .end(if result == Error::Closed { Ok(()) } else { Err(result) });
    if let Some(reply) = session.describe.take() {
        let _ = reply.send(Err(result));
    }
    if let Some(play) = session.play.take() {
        let _ = play.reply.send(Err(result));
    }
    end.send_replace(Some(result));
    reader_task.0.abort();
    let _ = (&mut reader_task.0).await;
}

impl<W: AsyncWrite + Unpin> Session<W> {
    async fn send(&mut self, event: WyomingEvent, payload: Vec<u8>) -> Result<(), Error> {
        timeout(
            self.opts.operation_timeout,
            wyoming_wire::write(&mut self.writer, &event, payload, self.opts.max_frame_bytes),
        )
        .await
        .map_err(|_| Error::Timeout)?
    }

    async fn run(
        &mut self,
        commands: &mut mpsc::Receiver<Command>,
        incoming: &mut mpsc::Receiver<Result<echo_contracts::wyoming::Event, Error>>,
    ) -> Error {
        loop {
            let result = tokio::select! {
                frame = incoming.recv() => self.received(frame.unwrap_or(Err(Error::Disconnected))),
                command = commands.recv() => self.command(command).await,
                frame = next_frame(&mut self.play) => self.audio(frame).await,
            };
            if let Err(error) = result {
                return error;
            }
        }
    }

    fn received(&mut self, frame: Result<echo_contracts::wyoming::Event, Error>) -> Result<(), Error> {
        match self.router.route(frame?)? {
            event @ WyomingEvent::Info(_) => {
                if let Some(reply) = self.describe.take() {
                    let _ = reply.send(Ok(event));
                }
            }
            WyomingEvent::Played(_) if self.play.as_ref().is_some_and(|p| p.stopped) => {
                if let Some(play) = self.play.take() {
                    let _ = play.reply.send(Ok(PlayOutcome::Played));
                }
                self.router.end(Ok(()));
            }
            _ => {}
        }
        Ok(())
    }

    async fn command(&mut self, command: Option<Command>) -> Result<(), Error> {
        match command {
            Some(Command::Describe(reply)) => {
                if self.describe.is_some() {
                    let _ = reply.send(Err(Error::Busy));
                } else {
                    self.describe = Some(reply);
                    self.send(WyomingEvent::Describe(Value::Null), vec![]).await?;
                }
            }
            Some(Command::Register(reply)) => {
                if !self.registered {
                    self.send(WyomingEvent::RunSatellite(Value::Null), vec![]).await?;
                    self.registered = true;
                }
                let _ = reply.send(Ok(()));
            }
            Some(Command::Play(format, frames, reply)) => {
                if self.play.is_some() {
                    let _ = reply.send(Err(Error::Busy));
                } else {
                    self.play = Some(Playback {
                        format: format.clone(),
                        frames,
                        reply,
                        stopped: false,
                    });
                    self.send(WyomingEvent::AudioStart(format), vec![]).await?;
                }
            }
            Some(Command::Close(reply)) => {
                let _ = reply.send(Ok(()));
                return Err(Error::Closed);
            }
            None => return Err(Error::Closed),
        }
        Ok(())
    }

    async fn audio(&mut self, payload: Option<Vec<u8>>) -> Result<(), Error> {
        let Some(play) = &mut self.play else {
            return Ok(());
        };
        if play.reply.is_closed() {
            return Err(Error::Closed);
        }
        let event = if payload.is_some() {
            WyomingEvent::AudioChunk(AudioChunkData {
                rate: play.format.rate,
                width: play.format.width,
                channels: play.format.channels,
                timestamp: None,
                extra: play.format.extra.clone(),
            })
        } else {
            play.stopped = true;
            WyomingEvent::AudioStop(Value::Null)
        };
        self.send(event, payload.unwrap_or_default()).await
    }
}

async fn next_frame(play: &mut Option<Playback>) -> Option<Vec<u8>> {
    match play {
        Some(play) if !play.stopped => play.frames.recv().await,
        _ => std::future::pending().await,
    }
}
