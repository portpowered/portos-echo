//! T1: reply generations retired by a truncated done or a user barge, and
//! a reply whose audio stalls in mid-speech.
use super::{cases::Stalled, *};
use crate::bridge_runtime::bridge_pacing::BARGE_QUIET;

#[path = "controls/tests/voice_barge_stop.rs"]
mod stop_cases;
#[path = "controls/tests/voice_barge_yield.rs"]
mod yield_cases;

/// T1: a late barge must not leave its replacement on the retired deadline.
#[tokio::test(start_paused = true)]
async fn voice_barge_replacement_gets_a_fresh_response_window_after_settled_cut() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 640);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let start = Instant::now();
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        options(),
        std::future::pending(),
        |_| Ok(()),
    ));
    send(&tx, [said("old reply")]).await;
    pending(work.as_mut()).await;
    tokio::time::advance(Duration::from_secs(9)).await;
    send(&tx, [MediaEvent::Control(Event::SpeechStarted)]).await;
    pending(work.as_mut()).await;
    assert_eq!(command_count(&observer, false), 0);
    tokio::time::advance(BARGE_QUIET).await;
    pending(work.as_mut()).await;
    assert_eq!(command_count(&observer, false), 1);
    observer.finish(Ok(()));
    pending(work.as_mut()).await;
    assert_eq!(command_count(&observer, true), 2);
    // Cross the original response deadline, but stay in the replacement budget.
    tokio::time::advance(Duration::from_secs(3)).await;
    pending(work.as_mut()).await;
    send(&tx, [said("replacement")]).await;
    send(&tx, audible(3)).await;
    send(&tx, [done()]).await;
    pending(work.as_mut()).await;
    assert_eq!(frames(&observer).len(), 3);
    assert_eq!(observer.commands().last(), Some(&Command::Stop));
    tokio::time::advance(Duration::from_millis(400)).await;
    pending(work.as_mut()).await;
    observer.finish(Ok(()));
    let stats = work.await.unwrap();
    assert_eq!(stats.reply_sent, 3);
    assert_eq!(stats.microphone_frames, 2);
    assert_eq!(command_count(&observer, false), 1);
    assert_eq!(observer.commands().last(), Some(&Command::EndRun));
    assert_eq!(start.elapsed(), Duration::from_millis(12_400) + BARGE_QUIET);
}

#[tokio::test(start_paused = true)]
async fn voice_late_audible_media_after_done_is_discarded() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    tx.send(Ok(done())).await.unwrap();
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        options(),
        std::future::pending(),
        |_| Ok(()),
    ));
    pending(work.as_mut()).await;
    tx.send(Ok(MediaEvent::Audio(Box::new([123; 320])))).await.unwrap();
    pending(work.as_mut()).await;
    observer.finish(Ok(()));
    assert_eq!(work.await.unwrap().reply_received, 1);
    assert_eq!(frames(&observer).len(), 0);
    assert!(!observer.commands().contains(&Command::Cut(0)));
}
/// T1: a real cut receipt must settle before the replacement playback starts.
#[tokio::test(start_paused = true)]
async fn voice_barge_retires_old_done_and_pcm_then_plays_the_same_text_answer() {
    for (done_first, signal) in [(false, 0), (true, 0), (false, 1), (true, 2)] {
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = client.events().unwrap();
        wake(&observer, 640);
        let mic = microphone(&mut events).await;
        let (mut port, tx) = port();
        let mut work = Box::pin(run(
            &client,
            &mut events,
            mic,
            &mut port,
            options(),
            std::future::pending(),
            |_| Ok(()),
        ));
        send(&tx, [said("Yes")]).await;
        let old_frames = if done_first && signal == 2 { 3 } else { 12 };
        send(&tx, audible(old_frames)).await;
        pending(work.as_mut()).await;
        tick(work.as_mut(), usize::from(old_frames == 3) * 5).await; // under the prime: held 100 ms
        assert_eq!(frames(&observer).len(), (old_frames - 1).min(10));
        if done_first {
            send(&tx, [same_text_done()]).await;
            pending(work.as_mut()).await;
        }
        assert_drained_reply(&observer, old_frames);
        send(&tx, [heard(signal)]).await;
        // A reply that is done is cut at once. One the model left open is
        // held from the device through the quiet stretch first.
        pending(work.as_mut()).await;
        tick(work.as_mut(), usize::from(!done_first) * 25).await;
        let old_sent = old_frames.min(10);
        assert_eq!(command_count(&observer, false), 1);
        assert_eq!(command_count(&observer, true), 1);
        observer.finish(Ok(()));
        pending(work.as_mut()).await;
        assert_eq!(command_count(&observer, true), 2);
        send(&tx, [same_text_done(), MediaEvent::Control(Event::SpeechStarted)]).await;
        send(&tx, audible(2)).await;
        send(&tx, [MediaEvent::Audio(Box::new([0; 320]))]).await;
        pending(work.as_mut()).await;
        assert_eq!(frames(&observer).len(), old_sent);
        send(&tx, [said("Yes")]).await;
        send(&tx, audible(3)).await;
        send(&tx, [same_text_done()]).await;
        pending(work.as_mut()).await;
        assert_eq!(frames(&observer).len(), old_sent + 3);
        assert_eq!(observer.commands().last(), Some(&Command::Stop));
        observer.finish(Ok(()));
        let stats = work.await.unwrap();
        assert_eq!(stats.reply_received, (old_frames + 6) as u64);
        assert_eq!(stats.reply_sent, (old_sent + 3) as u64);
        assert_eq!((stats.microphone_frames, command_count(&observer, false)), (2, 1));
    }
}

/// T3: under the paused clock, a barge cut withdraws the reply frames the
/// device queue discarded, so the next clock step settles instead of waiting.
#[tokio::test(start_paused = true)]
async fn voice_barge_cut_withdraws_discarded_reply_frames_from_the_paused_clock() {
    use echo_contracts::secret::Secret;
    let clock = crate::bridge_mcp::bridge_clock::receipt::Context::for_voice_test();
    let (_states, input) = tokio::sync::mpsc::channel(4);
    let address = ([127, 0, 0, 1], 0).into();
    let server = echo_screen_server::serve(address, Secret::new("synthetic-clock-pair".into()), input);
    let server = std::sync::Arc::new(server.await.unwrap());
    clock.screen(&server).unwrap();
    let observer = Observer::default();
    let client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let (sink, reply) = client.reply_channel(std::num::NonZeroUsize::new(3).unwrap());
    let result = std::cell::Cell::new(None);
    let ceiling = std::time::Duration::from_secs(5);
    let mut playback = Box::pin(async {
        result.set(Some(
            client
                .play_with_timeout(crate::bridge_audio::format(24_000), reply, ceiling)
                .await,
        ));
        Ok(())
    });
    pending(playback.as_mut()).await;
    // Accepted by the device queue and counted by the clock, never written.
    for value in 1..=3 {
        sink.send(vec![value; 960]).await.unwrap();
        clock.submitted("reply").unwrap();
    }
    observer.finish(Ok(()));
    let source = Source::new(200, false).unwrap();
    let second = std::time::Duration::from_secs(1);
    let cut = voice_source::cut_generation(&client, playback, &result, second, &source, true);
    assert_eq!(clock.owning(cut).await, Ok(()));
    assert_eq!(command_count(&observer, false), 1);
    assert!(observer.commands().contains(&Command::Cut(3)));
    assert_eq!(frames(&observer).len(), 0);
    let step = clock.next_step().await.unwrap();
    assert_eq!(step["pcm"], serde_json::json!([]));
    drop(sink);
    std::sync::Arc::try_unwrap(server)
        .ok()
        .unwrap()
        .shutdown()
        .await
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn voice_barge_without_cut_receipt_never_opens_replacement_playback() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    send(
        &tx,
        [
            said("old"),
            MediaEvent::Control(Event::SpeechStarted),
            said("new"),
            done(),
        ],
    )
    .await;
    assert_eq!(
        run(
            &client,
            &mut events,
            mic,
            &mut port,
            options(),
            std::future::pending(),
            |_| Ok(())
        )
        .await,
        Err(Error::Deadline)
    );
    assert_eq!(command_count(&observer, true), 1);
    assert_eq!(frames(&observer).len(), 0);
}

/// A sign of user speech: the speech-start item, or a transcript fragment.
fn heard(signal: usize) -> MediaEvent {
    MediaEvent::Control(if signal == 0 {
        Event::SpeechStarted
    } else {
        Event::Transcript {
            role: "user".into(),
            text: "new question".into(),
            done: signal == 2,
        }
    })
}

fn command_count(observer: &Observer, starts: bool) -> usize {
    observer
        .commands()
        .iter()
        .filter(|command| {
            if starts {
                matches!(command, Command::Start(..))
            } else {
                matches!(command, Command::Cut(_))
            }
        })
        .count()
}

#[tokio::test(start_paused = true)]
async fn voice_barge_is_not_armed_while_a_dispatch_is_in_flight() {
    let (mut port, _tx) = port();
    let mut source = Source::new(200, false).unwrap();
    // A dispatch in flight is thinking: user speech arms nothing, before or
    // during its filler. Barge-in returns once it completes.
    for event in [turn(true), said("One sec")] {
        media(event, &mut port, &mut source, &mut |_| Ok(())).await.unwrap();
        source.heard_user(BargeTrigger::SpeechStarted, "", false);
        assert!(source.delegation.in_flight() && source.barge_due().is_none());
    }
    assert!(source.controller.speaking());
    assert_eq!(port.retired, 0);
    media(turn(false), &mut port, &mut source, &mut |_| Ok(()))
        .await
        .unwrap();
    source.heard_user(BargeTrigger::SpeechStarted, "", false);
    source.interrupt(&mut port).unwrap();
    assert!(source.controller.speaking());
    tokio::time::advance(BARGE_QUIET).await;
    source.interrupt(&mut port).unwrap();
    assert!(source.controller.cutting());
    assert_eq!(port.retired, 1);
}

#[tokio::test(start_paused = true)]
async fn voice_barge_discards_unflushed_converter_and_pending_backpressure_frame() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mut mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let mut source = Source::new(200, false).unwrap();
    source.controller.microphone_ended();
    media(said("old"), &mut port, &mut source, &mut |_| Ok(()))
        .await
        .unwrap();
    source.audio(&[123; 320]).unwrap();
    assert!(!source.reply.has_frames());
    let mut pending_frame = Some(vec![77; 960]);
    tx.send(Ok(MediaEvent::Control(Event::SpeechStarted))).await.unwrap();
    // This step only arms the barge-in; the quiet stretch fires it below.
    let (sink, observe) = (Some(&Stalled), &mut |_| Ok(()));
    let step = step(
        &mut events,
        &mut mic,
        &mut port,
        sink,
        &mut source,
        &mut pending_frame,
        observe,
    );
    step.await.unwrap();
    assert!(!source.controller.cutting());
    assert_eq!(port.retired, 0);
    tokio::time::advance(BARGE_QUIET).await;
    let mut sink = Some(Stalled);
    assert_eq!(
        drive(
            (&client, &mut events, &mut mic),
            &mut port,
            &mut sink,
            (&mut source, &mut None),
            &options(),
            &mut |_| Ok(())
        )
        .await,
        Err(Error::Control)
    );
    assert!(source.controller.cutting());
    assert_eq!(port.retired, 1);
    assert!(sink.is_some());
    assert!(!source.reply.finished());
    assert!(!source.reply.has_frames());
    assert_retired_and_replacement(&mut source, &mut port).await;
}
async fn assert_retired_and_replacement(source: &mut Source, port: &mut Port) {
    source.controller.cut_settled();
    media(done(), port, source, &mut |_| Ok(())).await.unwrap();
    source.audio(&[123; 320]).unwrap();
    source.audio(&[0; 320]).unwrap();
    assert!(!source.reply.has_frames());
    assert!(!source.reply.finished());
    media(said("Yes"), port, source, &mut |_| Ok(())).await.unwrap();
    source.audio(&[456; 320]).unwrap();
    media(done(), port, source, &mut |_| Ok(())).await.unwrap();
    assert!(source.reply.finished());
    let mut fresh = crate::bridge_codex::ReplyAudio::new(200).unwrap();
    fresh.push(&[456; 320]).unwrap();
    fresh.finish().unwrap();
    assert_eq!(
        source.reply.next_frame().await.unwrap(),
        fresh.next_frame().await.unwrap()
    );
    assert!(!source.reply.has_frames());
}
fn same_text_done() -> MediaEvent {
    MediaEvent::Control(Event::ReplyDone { text: "Yes.".into() })
}

fn pcm(frame: &[u8]) -> Vec<i16> {
    let (samples, _) = frame.as_chunks::<2>();
    samples.iter().map(|sample| i16::from_le_bytes(*sample)).collect()
}

/// T1: the call stops delivering in mid-speech. The Echo gets a fade-out
/// before it runs dry and a fade-in when the audio is back, not a cut.
#[tokio::test(start_paused = true)]
async fn voice_reply_stalled_mid_speech_fades_out_and_back_in() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        options(),
        std::future::pending(),
        |_| Ok(()),
    ));
    send(&tx, [said("It's")]).await;
    send(&tx, audible(12)).await;
    pending(work.as_mut()).await;
    tick(work.as_mut(), 1).await;
    // Eleven frames are out (the resampler holds a tail): 220 ms of audio.
    assert_eq!(frames(&observer).len(), 11);
    tick(work.as_mut(), 8).await;
    assert_eq!(frames(&observer).len(), 11);
    // 20 ms before the Echo is dry it gets the last frame, mirrored and faded.
    tick(work.as_mut(), 1).await;
    let heard = frames(&observer);
    assert_eq!(heard.len(), 12);
    let (last, faded) = (pcm(&heard[10]), pcm(&heard[11]));
    assert_eq!((last[479], faded.len()), (123, 480));
    assert_eq!((faded[0], faded[240], faded[479]), (123, 61, 0));
    // Dry for 80 ms, then the rest arrives: the first frame fades in.
    tick(work.as_mut(), 6).await;
    assert_eq!(frames(&observer).len(), 12);
    send(&tx, audible(12)).await;
    send(&tx, [done()]).await;
    pending(work.as_mut()).await;
    tick(work.as_mut(), 4).await;
    let heard = frames(&observer);
    assert_eq!(heard.len(), 25);
    let resumed = pcm(&heard[12]);
    assert_eq!((resumed[0], resumed[240], resumed[479]), (0, 61, 122));
    assert_eq!(pcm(&heard[13])[240], 123);
    observer.finish(Ok(()));
    let stats = work.await.unwrap();
    assert_eq!((stats.reply_received, stats.reply_sent), (24, 25));
    let quality = stats.quality;
    assert_eq!((quality.underruns, quality.underrun_max_ms), (1, 80));
    assert_eq!((quality.reprimes, quality.concealment_ms, quality.fade_ins), (1, 20, 1));
    // The second burst came 320 ms after the first, which had come all at
    // once: late enough to ask for the most cushion there is.
    assert_eq!(
        (quality.target_min_ms, quality.target_max_ms, quality.target_final_ms),
        (100, 320, 320)
    );
    assert_eq!(quality.drained_frames, 0);
}
