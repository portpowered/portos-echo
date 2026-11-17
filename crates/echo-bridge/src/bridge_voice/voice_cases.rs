use super::*;

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
    source.capture(Some(frame()), &port).unwrap();
    assert_eq!(source.stats.microphone_chunks, 1);
    assert_eq!(source.stats.microphone_frames, 0);
    assert!(source.microphone.is_none());
    source.muted = false;
    port.reject = Some(1);
    assert_eq!(source.capture(Some(frame()), &port), Err(Error::Unavailable));
    assert_eq!(source.stats.microphone_frames, 1);
    assert_eq!(port.sent.lock().unwrap().len(), 1);
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
