use super::*;

#[tokio::test(start_paused = true)]
async fn reply_preempts_earcon_and_waits_for_its_receipt_before_owning_speaker() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 640);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    port.ready = (0..10).map(|_| [5000; 320]).collect();
    let mut notices = Vec::new();
    let mut stats = None;
    let work = super::super::earcon::scope(&client, true, async {
        super::super::earcon::start()?;
        stats = Some(
            run(
                &client,
                &mut events,
                mic,
                &mut port,
                options(),
                std::future::pending(),
                |notice| {
                    notices.push(preemption_notice(&notice));
                    Ok(())
                },
            )
            .await?,
        );
        Ok(())
    });
    let mut work = Box::pin(work);
    pending(work.as_mut()).await;
    assert_eq!(frames(&observer).len(), 1);
    tx.send(Ok(done())).await.unwrap();
    pending(work.as_mut()).await;
    assert_eq!(frames(&observer).len(), 1);
    assert_speaker_operations(&observer, 1, 1);
    observer.finish(Ok(()));
    pending(work.as_mut()).await;
    assert_speaker_operations(&observer, 2, 1);
    assert_eq!(frames(&observer).len(), 11);
    assert_eq!(observer.commands().last(), Some(&Command::Stop));
    tokio::time::advance(Duration::from_millis(400)).await;
    pending(work.as_mut()).await;
    observer.finish(Ok(()));
    assert_eq!(work.await, Ok(()));
    assert_preempted_reply(&stats.unwrap(), &notices, &observer);
    assert_eq!(frames(&observer)[1..].concat(), reply_without_earcon().await);
}

async fn reply_without_earcon() -> Vec<u8> {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 640);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    port.ready = (0..10).map(|_| [5000; 320]).collect();
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
    assert_eq!(frames(&observer).len(), 10);
    assert_eq!(observer.commands().last(), Some(&Command::Stop));
    tokio::time::advance(Duration::from_millis(400)).await;
    pending(work.as_mut()).await;
    observer.finish(Ok(()));
    assert_eq!(work.await.unwrap().reply_sent, 10);
    frames(&observer).concat()
}

fn preemption_notice(notice: &Notice) -> &'static str {
    match notice {
        Notice::EarconPlayed(duration) => {
            assert_eq!(*duration, Duration::from_millis(20));
            "earcon"
        }
        Notice::ReplyStarted => "speaking",
        Notice::ReplyPaused => "paused",
        Notice::ReplyPlayed => "played",
        Notice::Transcript { .. } => "transcript",
        Notice::Device(_) => "device",
    }
}

fn assert_speaker_operations(observer: &Observer, starts: usize, cuts: usize) {
    assert_eq!(
        observer
            .commands()
            .iter()
            .filter(|c| matches!(c, Command::Start(..)))
            .count(),
        starts
    );
    assert_eq!(
        observer
            .commands()
            .iter()
            .filter(|c| matches!(c, Command::Cut(_)))
            .count(),
        cuts
    );
}

fn assert_preempted_reply(stats: &Stats, notices: &[&str], observer: &Observer) {
    assert_eq!(stats.reply_received, 10);
    assert_eq!(stats.reply_sent, 10);
    assert_eq!(stats.reply_dropped, 0);
    assert_eq!(stats.microphone_frames, 2);
    assert_eq!(stats.dropped_chunks, 0);
    assert_eq!(stats.microphone_tail, 0);
    let pcm = frames(observer);
    assert_eq!(pcm[1..].concat().len(), 9600);
    assert_eq!(notices.iter().filter(|n| **n == "earcon").count(), 1);
    assert_eq!(notices.iter().filter(|n| **n == "played").count(), 1);
    let earcon = notices.iter().position(|n| *n == "earcon").unwrap();
    let speaking = notices.iter().position(|n| *n == "speaking").unwrap();
    assert!(earcon < speaking);
}

#[tokio::test(start_paused = true)]
async fn voice_second_run_reuses_port_without_old_pcm_or_reply_state() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    let (mut port, tx) = port();
    for count in [2, 1] {
        wake(&observer, 640);
        let mic = microphone(&mut events).await;
        port.ready = (0..count).map(|_| [0; 320]).collect();
        tx.send(Ok(done())).await.unwrap();
        let before = frames(&observer).len();
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
        assert_eq!(frames(&observer).len() - before, count);
        assert_eq!(observer.commands().last(), Some(&Command::Stop));
        observer.finish(Ok(()));
        let stats = work.await.unwrap();
        assert_eq!(stats.reply_sent, count as u64);
        assert_eq!(stats.reply_received, count as u64);
        assert_eq!(stats.reply_dropped, 0);
        assert_eq!(stats.microphone_frames, 2);
    }
    assert_eq!(port.sent.lock().unwrap().len(), 4);
}

#[tokio::test(start_paused = true)]
async fn voice_premature_played_and_failed_playback_never_succeed() {
    for outcome in [Ok(()), Err(echo_link::Error::Transport)] {
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = client.events().unwrap();
        wake(&observer, 0);
        let mic = microphone(&mut events).await;
        let (mut port, _tx) = port();
        let mut played = 0;
        let mut work = Box::pin(run(
            &client,
            &mut events,
            mic,
            &mut port,
            options(),
            std::future::pending(),
            |notice| {
                if matches!(notice, Notice::ReplyPlayed) {
                    played += 1;
                }
                Ok(())
            },
        ));
        pending(work.as_mut()).await;
        observer.finish(outcome);
        assert_eq!(work.await, Err(Error::Wyoming));
        assert_eq!(played, 0);
        assert!(observer.commands().contains(&Command::Cut(0)));
    }
}

#[tokio::test(start_paused = true)]
async fn voice_initial_mute_and_failed_send_keep_exact_accepted_capture_counts() {
    let frame = || MicFrame {
        format: crate::bridge_audio::format(16_000),
        payload: vec![0; 1280],
    };
    let (mut port, _tx) = port();
    let mut source = Source::new(200, true).unwrap();
    assert!(source.capture(Some(frame()), &port).unwrap());
    assert_eq!(source.stats.microphone_chunks, 1);
    assert_eq!(source.stats.microphone_frames, 0);
    assert!(source.microphone.is_none());
    source.muted = false;
    port.reject = Some(1);
    assert_eq!(source.capture(Some(frame()), &port), Err(Error::Unavailable));
    assert_eq!(source.stats.microphone_frames, 1);
    assert_eq!(port.sent.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn voice_duplicate_unmute_preserves_pcm_but_a_transition_discards_partial_capture() {
    let frame = |value: i16| MicFrame {
        format: crate::bridge_audio::format(16_000),
        payload: value.to_le_bytes().repeat(160),
    };
    let (port, _tx) = port();
    let mut source = Source::new(200, false).unwrap();
    assert!(!source.capture(Some(frame(11)), &port).unwrap());
    device(&LinkEvent::<()>::Mute(false), false, &mut source, &mut |_| Ok(())).unwrap();
    assert!(!source.capture(Some(frame(22)), &port).unwrap());
    assert_eq!(port.sent.lock().unwrap()[0][..160], [11; 160]);
    assert_eq!(port.sent.lock().unwrap()[0][160..], [22; 160]);
    assert!(!source.capture(Some(frame(33)), &port).unwrap());
    device(&LinkEvent::<()>::Mute(true), false, &mut source, &mut |_| Ok(())).unwrap();
    assert!(source.capture(Some(frame(44)), &port).unwrap());
    device(&LinkEvent::<()>::Mute(false), false, &mut source, &mut |_| Ok(())).unwrap();
    assert!(!source.capture(Some(frame(55)), &port).unwrap());
    assert!(!source.capture(None, &port).unwrap());
    assert_eq!(source.stats.microphone_frames, 1);
    assert_eq!(source.stats.microphone_chunks, 5);
    assert_eq!(source.stats.microphone_tail, 160);
    assert_eq!(port.sent.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn voice_muted_capture_reports_each_suppression_and_propagates_observer_failure() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 320);
    let mut mic = microphone(&mut events).await;
    let (mut port, _tx) = port();
    let mut source = Source::new(200, true).unwrap();
    let mut dropped = 0;
    assert_eq!(
        step(
            &mut events,
            &mut mic,
            &mut port,
            Some(&Stalled),
            &mut source,
            &mut None,
            &mut |notice| {
                match notice {
                    Notice::Device(LinkEvent::MicrophoneDropped(count)) => dropped += count,
                    _ => unreachable!(),
                }
                Err(Error::Screen)
            },
        )
        .await,
        Err(Error::Screen)
    );
    assert_eq!(dropped, 1);
    assert_eq!(source.stats.microphone_chunks, 1);
    assert_eq!(source.stats.microphone_frames, 0);
    assert_eq!(port.sent.lock().unwrap().len(), 0);
}

#[tokio::test(start_paused = true)]
async fn voice_wire_mute_drains_preceding_capture_and_counts_only_suppressed_chunks() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    observer.wake_with_mute(
        "Computer",
        (1_i16..=4)
            .map(|value| MicFrame {
                format: crate::bridge_audio::format(16_000),
                payload: value.to_le_bytes().repeat(320),
            })
            .collect(),
        2,
    );
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let sent = port.sent.clone();
    tx.send(Ok(done())).await.unwrap();
    let mut privacy = Vec::new();
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        options(),
        std::future::pending(),
        |notice| {
            match notice {
                Notice::Device(LinkEvent::Mute(muted)) => privacy.push((muted, 0)),
                Notice::Device(LinkEvent::MicrophoneDropped(count)) => privacy.push((true, count)),
                _ => (),
            }
            Ok(())
        },
    ));
    pending(work.as_mut()).await;
    assert_eq!(*sent.lock().unwrap(), [[1; 320], [2; 320]]);
    observer.mute(false);
    pending(work.as_mut()).await;
    observer.finish(Ok(()));
    let stats = work.await.unwrap();
    assert_eq!(stats.received_chunks, 4);
    assert_eq!(stats.dropped_chunks, 2);
    assert_eq!(stats.microphone_chunks, 2);
    assert_eq!(stats.microphone_frames, 2);
    assert_eq!(privacy, [(true, 0), (true, 2), (false, 0)]);
    assert_eq!(*sent.lock().unwrap(), [[1; 320], [2; 320]]);
}

struct Stalled;
impl ReplySink for Stalled {
    fn send(&self, _: Vec<u8>) -> impl Future<Output = Result<(), echo_link::Error>> + Send {
        std::future::pending()
    }
}

#[tokio::test(start_paused = true)]
async fn voice_backpressure_keeps_pending_pcm_and_continues_control() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mut mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let mut source = Source::new(200, false).unwrap();
    source.microphone_finished = true;
    let mut pending = Some(vec![12; 960]);
    let mut notices = Vec::new();
    for text in ["first", "second"] {
        tx.send(Ok(MediaEvent::Control(Event::Transcript {
            role: "user".into(),
            text: text.into(),
            done: false,
        })))
        .await
        .unwrap();
        step(
            &mut events,
            &mut mic,
            &mut port,
            Some(&Stalled),
            &mut source,
            &mut pending,
            &mut |notice| {
                if let Notice::Transcript { text, .. } = notice {
                    notices.push(text);
                }
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(pending, Some(vec![12; 960]));
        assert_eq!(source.stats.reply_sent, 0);
    }
    assert_eq!(notices, vec!["first", "second"]);
}

#[tokio::test(start_paused = true)]
async fn voice_overflow_counts_oldest_evictions_and_sends_only_retained_frames() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    port.ready = (0..300).map(|_| [0; 320]).collect();
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
    assert_eq!(frames(&observer).len(), 10);
    for count in 11..=128 {
        tokio::time::advance(Duration::from_millis(20)).await;
        pending(work.as_mut()).await;
        assert_eq!(frames(&observer).len(), count);
    }
    assert_eq!(observer.commands().last(), Some(&Command::Stop));
    observer.finish(Ok(()));
    let stats = work.await.unwrap();
    assert_eq!(stats.reply_received, 300);
    assert_eq!(stats.reply_sent, 128);
    assert_eq!(stats.reply_dropped, 172);
    assert_eq!(frames(&observer).concat(), vec![0; 128 * 960]);
}

#[tokio::test(start_paused = true)]
async fn shutdown_during_earcon_collects_only_the_started_speaker_owner() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, _tx) = port();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let mut notices = Vec::new();
    let work = super::super::earcon::scope(&client, true, async {
        super::super::earcon::start()?;
        run(
            &client,
            &mut events,
            mic,
            &mut port,
            options(),
            async {
                stopped.await.unwrap();
            },
            |notice| {
                notices.push(preemption_notice(&notice));
                Ok(())
            },
        )
        .await
        .map(|_| ())
    });
    let mut work = Box::pin(work);
    pending(work.as_mut()).await;
    assert_eq!(frames(&observer).len(), 1);
    stop.send(()).unwrap();
    pending(work.as_mut()).await;
    observer.finish(Ok(()));
    assert_eq!(work.await, Err(Error::Control));
    assert_speaker_operations(&observer, 1, 1);
    assert_eq!(frames(&observer).len(), 1);
    assert_eq!(notices, [] as [&str; 0]);
    client.close().await.unwrap();
    assert_eq!(observer.commands().last(), Some(&Command::Close));
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
        send(&tx, audible(12)).await;
        pending(work.as_mut()).await;
        assert_eq!(frames(&observer).len(), 10);
        if done_first {
            send(&tx, [same_text_done()]).await;
            pending(work.as_mut()).await;
        }
        let signal = if signal == 0 {
            Event::SpeechStarted
        } else {
            Event::Transcript {
                role: "user".into(),
                text: "new question".into(),
                done: signal == 2,
            }
        };
        send(&tx, [MediaEvent::Control(signal)]).await;
        pending(work.as_mut()).await;
        assert_eq!(command_count(&observer, false), 1);
        assert_eq!(command_count(&observer, true), 1);
        // Media already admitted to the old generation and pending PCM are retired.
        observer.finish(Ok(()));
        pending(work.as_mut()).await;
        assert_eq!(command_count(&observer, true), 2);
        send(&tx, [same_text_done(), MediaEvent::Control(Event::SpeechStarted)]).await;
        send(&tx, audible(2)).await;
        send(&tx, [MediaEvent::Audio(Box::new([0; 320]))]).await;
        pending(work.as_mut()).await;
        assert_eq!(frames(&observer).len(), 10);
        send(&tx, [said("Yes")]).await;
        send(&tx, audible(3)).await;
        send(&tx, [same_text_done()]).await;
        pending(work.as_mut()).await;
        assert_eq!(frames(&observer).len(), 13);
        assert_eq!(observer.commands().last(), Some(&Command::Stop));
        observer.finish(Ok(()));
        let stats = work.await.unwrap();
        assert_eq!(stats.reply_received, 18);
        assert_eq!(stats.reply_sent, 13);
        assert_eq!(stats.microphone_frames, 2);
        assert_eq!(command_count(&observer, false), 1);
    }
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
async fn voice_barge_discards_unflushed_converter_and_pending_backpressure_frame() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mut mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let mut source = Source::new(200, false).unwrap();
    media(turn(true), &mut port, &mut source, &mut |_| Ok(()))
        .await
        .unwrap();
    source.interrupt(&mut port).unwrap();
    assert_eq!(port.retired, 0);
    assert_eq!(source.state, ReplyState::Cutting);
    source = Source::new(200, false).unwrap();
    source.microphone_finished = true;
    media(said("old"), &mut port, &mut source, &mut |_| Ok(()))
        .await
        .unwrap();
    source.audio(&[123; 320]).unwrap();
    assert!(!source.reply.has_frames());
    let mut pending_frame = Some(vec![77; 960]);
    tx.send(Ok(MediaEvent::Control(Event::SpeechStarted))).await.unwrap();
    step(
        &mut events,
        &mut mic,
        &mut port,
        Some(&Stalled),
        &mut source,
        &mut pending_frame,
        &mut |_| Ok(()),
    )
    .await
    .unwrap();
    assert_eq!(source.state, ReplyState::Cutting);
    assert_eq!(port.retired, 1);
    let mut sink = Some(Stalled);
    assert_eq!(
        drive(
            &mut events,
            &mut mic,
            &mut port,
            &mut sink,
            (&mut source, &mut None),
            &options(),
            &mut |_| Ok(())
        )
        .await,
        Err(Error::Control)
    );
    assert!(sink.is_some());
    assert!(!source.reply.finished());
    assert!(!source.reply.has_frames());
    assert_retired_and_replacement(&mut source, &mut port).await;
}

async fn assert_retired_and_replacement(source: &mut Source, port: &mut Port) {
    source.state = ReplyState::Retired;
    media(done(), port, source, &mut |_| Ok(())).await.unwrap();
    source.audio(&[123; 320]).unwrap();
    source.audio(&[0; 320]).unwrap();
    assert!(!source.reply.has_frames());
    assert!(!source.reply.finished());
    media(said("Yes"), port, source, &mut |_| Ok(())).await.unwrap();
    source.audio(&[456; 320]).unwrap();
    media(done(), port, source, &mut |_| Ok(())).await.unwrap();
    assert!(source.reply.finished());
    assert_eq!(source.reply.next_frame().await.unwrap().len(), 960);
    assert!(!source.reply.has_frames());
}

fn same_text_done() -> MediaEvent {
    MediaEvent::Control(Event::ReplyDone { text: "Yes.".into() })
}
