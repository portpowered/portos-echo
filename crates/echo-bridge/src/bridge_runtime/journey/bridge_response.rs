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
    let producer = async {
        let result = produce(source, run, events, call, tx, lab, context).await;
        failure = stop_failed_producer(result, client, lab.0).await?;
        context.await_played(events, run, source, call).await
    };
    let (outcome, ()) = tokio::try_join!(async { play.await.map_err(Error::from) }, producer)?;
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
