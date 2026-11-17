use super::{
    Cancellation, Duration, EchoLink, Error, Instant, LinkEvents, LivePort, MicrophoneRun, NonZeroUsize, PlayOutcome,
    PlaybackContext, Source, Trace, bridge_audio, bridge_timing, json, produce, stop_failed_producer,
};

pub(in crate::bridge_runtime) async fn run<L: EchoLink>(
    client: &L,
    events: &mut L::Events,
    mut run: <L::Events as LinkEvents>::Run,
    call: &mut impl LivePort,
    trace: &mut Trace,
    id: &str,
    options: (
        bool,
        u16,
        u64,
        &super::super::bridge_screen::Screen,
        &mut super::super::bridge_delegation::Host,
        &super::super::bridge_kill::Kill<'_>,
    ),
) -> Result<(u64, bool), Error> {
    let mut source = Source::new()?;
    source.call_packets_base = options.2;
    let mut context = PlaybackContext {
        client,
        trace,
        id,
        lead_ms: options.1,
        screen: options.3,
        speaking: false,
        microphone: false,
        microphone_at_reply: None,
        played: false,
        cancellation: Cancellation::Response,
        cut: None,
        deadline: Instant::now() + bridge_timing::RESPONSE,
        delegation: options.4,
        kill: options.5,
    };
    let mut abandoned = false;
    loop {
        abandoned |= play(
            &mut context,
            &mut source,
            &mut run,
            events,
            call,
            (options.0, options.2),
        )
        .await?;
        if !context.continuing(&source) {
            if source.cancelled && source.sent > 0 {
                client.pause().await?;
                client.enable_capture().await?;
                client.probe().await?;
            }
            break;
        }
        // Reset only reply state. Keep forwarding the interrupting question
        // until the capture port reports its own completion.
        source.resume_reply()?;
        context.played = false;
        context.speaking = false;
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        while run.recv().await?.is_some() {}
        Ok::<_, Error>(())
    })
    .await
    .map_err(|_| Error::Deadline)??;
    source.record(context.trace, id, run.stats(), !source.cancelled && !abandoned)?;
    Ok((source.sent, !source.cancelled && !abandoned))
}

async fn play<L: EchoLink>(
    context: &mut PlaybackContext<'_, L>,
    source: &mut Source,
    run: &mut <L::Events as LinkEvents>::Run,
    events: &mut L::Events,
    call: &mut impl LivePort,
    lab: (bool, u64),
) -> Result<bool, Error> {
    let client = context.client;
    let (tx, rx) = client.reply_channel(NonZeroUsize::new(1).ok_or(Error::Audio)?);
    let play = client.play_with_timeout(
        bridge_audio::format(24_000),
        rx,
        bridge_timing::playback(context.kill.settings),
    );
    let mut failure = None;
    let mut tx = Some(tx);
    let producer = async {
        let result = produce(
            source,
            run,
            events,
            call,
            tx.as_ref().ok_or(Error::Wyoming)?,
            lab,
            context,
        )
        .await;
        // Cancellation owns its cut even after a failed receipt was consumed.
        let result = stop_failed_producer(result, client, lab.0 || source.cancelled).await;
        drop(tx.take());
        failure = result?;
        context.await_played(events, run, source, call).await
    };
    tokio::pin!(play);
    let completion = complete_playback(&mut play, producer).await;
    let outcome = context.finish_playback(play, completion).await?;
    if failure.is_some() && source.sent > 0 {
        // A failed response has no replacement answer to end continued capture.
        // Settle Played first, then return the same satellite to wake readiness.
        client.pause().await?;
        client.enable_capture().await?;
        client.probe().await?;
    }
    failure.map_or(Ok(()), Err)?;
    let abandoned = context.delegation.abandon().await;
    context.screen.presentation.clear_emotion()?;
    context.trace.record(
        "reply.played",
        Some(context.id),
        json!({"outcome":if outcome == PlayOutcome::Cut { "cut" } else { "played" }}),
    )?;
    Ok(abandoned)
}

enum Completion {
    Playback(Result<PlayOutcome, Error>),
    Producer(Result<(), Error>),
}
async fn complete_playback(
    play: impl std::future::Future<Output = Result<PlayOutcome, echo_link::Error>>,
    producer: impl std::future::Future<Output = Result<(), Error>>,
) -> Completion {
    tokio::pin!(play, producer);
    tokio::select! {
        biased;
        outcome = &mut play => {
            // Keep producer-owned failure cleanup and microphone service alive.
            let result = producer.await;
            Completion::Playback(outcome.map_err(Error::from).and_then(|outcome| result.map(|()| outcome)))
        },
        result = &mut producer => Completion::Producer(result),
    }
}

impl<L: EchoLink> PlaybackContext<'_, L> {
    async fn finish_playback(
        &mut self,
        play: impl std::future::Future<Output = Result<PlayOutcome, echo_link::Error>>,
        completion: Completion,
    ) -> Result<PlayOutcome, Error> {
        let result = match completion {
            Completion::Playback(result) => result,
            Completion::Producer(Ok(())) => play.await.map_err(Error::from),
            Completion::Producer(Err(error)) => {
                if self.cut.is_none() {
                    // Settled cuts keep the original Played failure ceiling.
                    return Err(error);
                }
                // Producer EOF must not drop the playback progress that
                // releases its outstanding receipt. Never recreate the cut.
                let (outcome, original) = tokio::join!(play, self.settle_failure(error));
                return Err(outcome.err().map_or(original, Error::from));
            }
        };
        match result {
            Ok(outcome) => Ok(outcome),
            Err(error) => Err(self.settle_failure(error).await),
        }
    }
    pub(super) async fn await_played(
        &mut self,
        events: &mut L::Events,
        run: &mut <L::Events as LinkEvents>::Run,
        source: &mut Source,
        call: &mut impl LivePort,
    ) -> Result<(), Error> {
        self.await_transport(events, run, source, call, false).await
    }
    pub(super) async fn await_transport(
        &mut self,
        events: &mut L::Events,
        run: &mut <L::Events as LinkEvents>::Run,
        source: &mut Source,
        call: &mut impl LivePort,
        cut_only: bool,
    ) -> Result<(), Error> {
        let kill = self.kill;
        let mut deadline = None;
        let mut ended = false;
        let mut arbiter = super::Arbiter::default();
        while self.cut.is_some() || (!cut_only && !self.played) {
            // Keep the issued cut alive while capture and device notifications
            // share turns. The Played budget still starts after its receipt.
            if self.cut.is_none() && deadline.is_none() {
                deadline = Some(Instant::now() + bridge_timing::CONTROL);
            }
            let cutting = self.cut.is_some();
            tokio::select! {
                work = arbiter.next(
                    async { if ended { std::future::pending().await } else { run.recv().await } },
                    async { if cutting { self.finish_cut().await } else { std::future::pending().await } },
                    std::future::pending::<std::convert::Infallible>(),
                    events.recv(),
                ) => match work {
                    super::Work::Device(event) => self.device(source, &event?.ok_or(Error::Wyoming)?)?,
                    super::Work::Control(result) => result?,
                    super::Work::Reply(never) => match never {},
                    super::Work::Microphone(frame) => {
                        if let Some(frame) = frame? {
                            if source.cancelled && self.cancellation == Cancellation::Response {
                                self.microphone(source, frame, call, false).await?;
                            }
                        } else {
                            ended = true;
                        }
                    },
                },
                () = async {
                    if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; }
                    else { std::future::pending().await }
                } => return Err(Error::Deadline),
                () = kill.wait(), if !source.cancelled => {
                    self.kill.sync(self.trace).await?;
                    self.cancellation = Cancellation::Shutdown;
                    source.cancelled = true;
                    self.delegation.abandon().await;
                    call.close();
                }
            }
        }
        Ok(())
    }
}
