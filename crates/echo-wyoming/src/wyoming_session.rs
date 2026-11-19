use crate::wyoming_events;
use crate::{ClientOptions, CutStats, Error, PlayOutcome, wyoming_route::Router, wyoming_wire};
use echo_contracts::wyoming::{AudioChunkData, AudioFormat, WyomingEvent};
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncWrite, BufReader},
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::timeout,
};

type Reply<T> = oneshot::Sender<Result<T, Error>>;
mod wyoming_playback;
pub(crate) mod wyoming_reconnect;
use wyoming_playback::Playback;

#[cfg(test)]
mod wyoming_cut_tests;

pub(crate) enum Command {
    Describe(Reply<WyomingEvent>),
    Register(Value, Reply<()>),
    Play(AudioFormat, mpsc::Receiver<Vec<u8>>, Reply<PlayOutcome>),
    // Retain the legacy internal command exercised by the unchanged W1 tests.
    #[cfg(test)]
    Cut(Reply<()>),
    CutWithStats(Reply<CutStats>),
    EndRun(Reply<()>),
    Error(String, String, Reply<()>),
    Pause(Reply<()>),
    Thinking(bool, Reply<()>),
    Close(Reply<()>),
}

struct Session<W> {
    writer: W,
    opts: ClientOptions,
    router: Router,
    describe: Option<Reply<WyomingEvent>>,
    play: Option<Playback>,
    registered: bool,
    registration: Value,
    paused: bool,
    closing: bool,
    run_open: Option<()>,
}

struct ReaderTask(JoinHandle<()>);
impl Drop for ReaderTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
pub(crate) async fn serve<T: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    io: T,
    opts: ClientOptions,
    mut commands: mpsc::Receiver<Command>,
    events: wyoming_events::Sender,
    end: watch::Sender<Option<Error>>,
) {
    let (_reset, mut reset) = watch::channel(0_u64);
    let mut enabled = None;
    let (result, _) = generation(io, opts, &mut commands, events, &mut reset, &mut enabled).await;
    end.send_replace(Some(result));
}

async fn generation<T: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    io: T,
    opts: ClientOptions,
    commands: &mut mpsc::Receiver<Command>,
    events: wyoming_events::Sender,
    reset: &mut watch::Receiver<u64>,
    enabled: &mut Option<Value>,
) -> (Error, bool) {
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
            generation: std::sync::Arc::new(()),
            muted: false,
            current: None,
            events,
            capacity: opts.mic_capacity,
        },
        opts,
        describe: None,
        play: None,
        registered: enabled.is_some(),
        registration: enabled.clone().unwrap_or(Value::Null),
        paused: false,
        closing: false,
        run_open: None,
    };
    reset.borrow_and_update();
    let work = async {
        if enabled.is_some() {
            if let Err(error) = session.register(session.registration.clone()).await {
                return error;
            }
            session.registered = true;
        }
        session.run(commands, &mut incoming).await
    };
    let result = tokio::select! {
        biased;
        _ = reset.changed() => Error::Timeout,
        result = work => result,
    };
    *enabled = session.registered.then_some(session.registration.clone());
    session.router.end(if session.closing { Ok(()) } else { Err(result) });
    if let Some(reply) = session.describe.take() {
        let _ = reply.send(Err(result));
    }
    if let Some(play) = session.play.take() {
        play.finish(Err(result));
    }
    reader_task.0.abort();
    let _ = (&mut reader_task.0).await;
    (result, session.closing)
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
            let frames = self
                .play
                .as_ref()
                .filter(|play| !play.stopped)
                .map(|play| play.frames.clone());
            let result = tokio::select! {
                biased;
                () = cancelled(&mut self.play) => self.cancel_playback().await,
                command = commands.recv() => self.command(command).await,
                frame = incoming.recv() => self.received(frame.unwrap_or(Err(Error::Disconnected))),
                frame = async { match frames { Some(queue) => queue.recv().await, None => std::future::pending().await } } => match frame {
                    Ok(frame) => self.audio(frame).await,
                    Err(error) => Err(error),
                },
            };
            if let Err(error) = result {
                return error;
            }
        }
    }

    async fn cancel_playback(&mut self) -> Result<(), Error> {
        self.end_device().await?;
        // A missing Played can expire the run's control deadline after the
        // complete stop was written. Retire that failed run without claiming
        // playback success or invalidating the connection for its error frame.
        // Cancellation before audio-stop still invalidates transport ownership.
        if !self.play.as_ref().is_some_and(|play| play.stopped) {
            return Err(Error::Closed);
        }
        if let Some(play) = self.play.take() {
            play.finish(Err(Error::Closed));
        }
        self.router.end(Ok(()));
        Ok(())
    }

    fn received(&mut self, frame: Result<echo_contracts::wyoming::Event, Error>) -> Result<(), Error> {
        let frame = frame?;
        if self.paused
            && matches!(
                frame.header.r#type.as_str(),
                "run-pipeline" | "audio-chunk" | "audio-start" | "audio-stop"
            )
        {
            frame.typed().map_err(|_| Error::Protocol)?;
            return Ok(());
        }
        match self.router.route(frame)? {
            WyomingEvent::RunPipeline(_) => self.run_open = Some(()),
            event @ WyomingEvent::Info(_) => {
                if let Some(reply) = self.describe.take() {
                    let _ = reply.send(Ok(event));
                }
            }
            WyomingEvent::Played(_) if self.play.as_ref().is_some_and(|p| p.stopped) => {
                if let Some(play) = self.play.take() {
                    let outcome = if play.cut {
                        PlayOutcome::Cut
                    } else {
                        PlayOutcome::Played
                    };
                    play.finish(Ok(outcome));
                }
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
            Some(Command::Register(data, reply)) => {
                if !self.registered {
                    self.register(data).await?;
                }
                let _ = reply.send(Ok(()));
            }
            Some(Command::Play(format, frames, reply)) => {
                if self.play.is_some() {
                    let _ = reply.send(Err(Error::Busy));
                } else {
                    self.play = Some(Playback::new(format.clone(), frames, reply, &self.opts));
                    self.send(WyomingEvent::AudioStart(format), vec![]).await?;
                }
            }
            #[cfg(test)]
            Some(Command::Cut(reply)) => {
                self.cut().await?;
                let _ = reply.send(Ok(()));
            }
            Some(Command::CutWithStats(reply)) => {
                let stats = self.cut().await?;
                let _ = reply.send(Ok(stats));
            }
            Some(Command::Pause(reply)) => {
                self.pause_capture().await?;
                let _ = reply.send(Ok(()));
            }
            Some(Command::Thinking(on, reply)) => self.thinking(on, reply).await?,
            Some(Command::EndRun(reply)) => self.end_run(reply).await?,
            Some(Command::Error(code, text, reply)) => {
                self.fail_run(code, text, reply).await?;
            }
            Some(Command::Close(reply)) => {
                self.end_device_if_active().await?;
                let _ = reply.send(Ok(()));
                self.closing = true;
                return Err(Error::Closed);
            }
            None => {
                self.end_device_if_active().await?;
                self.closing = true;
                return Err(Error::Closed);
            }
        }
        Ok(())
    }

    async fn register(&mut self, data: Value) -> Result<(), Error> {
        self.send(WyomingEvent::RunSatellite(data.clone()), vec![]).await?;
        self.registration = data;
        self.registered = true;
        self.paused = false;
        Ok(())
    }

    /// Display only: hassmic shows its thinking face until told otherwise.
    async fn thinking(&mut self, on: bool, reply: Reply<()>) -> Result<(), Error> {
        let data = serde_json::json!({"thinking":on});
        self.send(WyomingEvent::Other("hassmic-thinking".into(), data), vec![])
            .await?;
        let _ = reply.send(Ok(()));
        Ok(())
    }

    async fn pause_capture(&mut self) -> Result<(), Error> {
        if !self.paused {
            self.retire_playback().await?;
            self.end_device().await?;
            self.send(WyomingEvent::PauseSatellite(Value::Null), vec![]).await?;
            self.paused = true;
            self.registered = false;
            self.router.end(Ok(()));
        }
        Ok(())
    }

    async fn end_run(&mut self, reply: Reply<()>) -> Result<(), Error> {
        if self.play.is_some() {
            let _ = reply.send(Err(Error::Busy));
            return Ok(());
        }
        let result = if self.run_open.is_some() {
            self.end_device().await
        } else {
            Ok(())
        };
        if result.is_ok() {
            self.router.end(Ok(()));
        }
        let _ = reply.send(result);
        result
    }

    async fn retire_playback(&mut self) -> Result<(), Error> {
        if let Some(mut play) = self.play.take() {
            play.quiesce().await?;
            play.finish(Err(Error::Closed));
        }
        Ok(())
    }

    async fn cut(&mut self) -> Result<CutStats, Error> {
        let mut stats = CutStats::default();
        if let Some(play) = &mut self.play {
            stats.dropped_frames = play.quiesce().await?;
            // Source EOF stops writes, but the device can still be playing.
            // Until Played retires this owner, a first cut must retain capture
            // and receive its own Cut outcome even after the ordinary stop.
            if !play.cut {
                play.cut = true;
                play.stopped = true;
                self.send(WyomingEvent::Other("hassmic-flush".into(), Value::Null), vec![])
                    .await?;
            }
        }
        Ok(stats)
    }

    async fn end_device(&mut self) -> Result<(), Error> {
        self.send(WyomingEvent::Other("hassmic-end".into(), Value::Null), vec![])
            .await?;
        self.run_open = None;
        Ok(())
    }

    async fn end_device_if_active(&mut self) -> Result<(), Error> {
        if self.run_open.is_some() || self.play.is_some() {
            self.end_device().await?;
        }
        Ok(())
    }

    async fn fail_run(&mut self, code: String, text: String, reply: Reply<()>) -> Result<(), Error> {
        // A cut already quiesced its producer. Keep its outstanding Played
        // receipt alive when an unserved run is refused during shutdown.
        if !self.play.as_ref().is_some_and(|play| play.cut) {
            self.retire_playback().await?;
        }
        self.end_device().await?;
        self.send(
            WyomingEvent::Error(serde_json::json!({"code":code,"text":text})),
            vec![],
        )
        .await?;
        self.router.end(Ok(()));
        let _ = reply.send(Ok(()));
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

async fn cancelled(play: &mut Option<Playback>) {
    match play {
        Some(play) => play.reply.closed().await,
        None => std::future::pending().await,
    }
}
