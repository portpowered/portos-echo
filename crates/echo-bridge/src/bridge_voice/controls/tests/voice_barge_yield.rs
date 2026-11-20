//! T1: user speech during a reply holds it and cuts it only once the model has yielded.
use super::*;
use crate::bridge_runtime::bridge_pacing::BARGE_WINDOW;

const FRAME: Duration = Duration::from_millis(20);

fn cuts(observer: &Observer) -> usize {
    command_count(observer, false)
}

/// One call frame per 20 ms, as the peer delivers a reply in real time.
pub(super) async fn stream<F: Future>(
    work: &mut Pin<Box<F>>,
    tx: &mpsc::Sender<Result<MediaEvent, Error>>,
    count: usize,
    sample: i16,
) {
    for _ in 0..count {
        send(tx, [MediaEvent::Audio(Box::new([sample; 320]))]).await;
        tick(work.as_mut(), 1).await;
    }
}

/// T1, the device's dropout: the microphone hears speech while the model
/// talks on. Each sign of it used to cut the reply and drop audio until the
/// model's next transcript fragment. Now the reply pauses while the barge-in
/// is pending and then plays whole.
#[tokio::test(start_paused = true)]
async fn voice_user_speech_under_a_talking_model_cuts_nothing() {
    let talking = usize::try_from((BARGE_WINDOW + BARGE_QUIET * 2).as_millis() / FRAME.as_millis()).unwrap();
    for signal in 0..3 {
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
        send(&tx, [said("A long answer")]).await;
        stream(&mut work, &tx, 25, 123).await;
        // Two signs of speech; the model never pauses.
        for _ in 0..2 {
            send(&tx, [heard(signal)]).await;
            stream(&mut work, &tx, 10, 123).await;
        }
        // Held from the first sign: at most the frame already on its way.
        assert!(frames(&observer).len() <= 26);
        stream(&mut work, &tx, talking, 123).await;
        send(&tx, [done()]).await;
        // The held stretch plays late: wait for the reply to drain.
        for _ in 0..150 {
            if observer.commands().last() == Some(&Command::Stop) {
                break;
            }
            tick(work.as_mut(), 1).await;
        }
        assert_eq!(observer.commands().last(), Some(&Command::Stop));
        observer.finish(Ok(()));
        let stats = work.await.unwrap();
        let received = 45 + talking;
        assert_eq!(cuts(&observer), 0);
        assert_eq!(frames(&observer).len(), received);
        assert_eq!(
            (stats.reply_received, stats.reply_sent),
            (received as u64, received as u64)
        );
        let quality = stats.quality;
        assert_eq!((quality.cuts, quality.cut_trigger, quality.holds), (0, None, 1));
        assert_eq!((quality.discarded_cut, quality.discarded_audible), (0, 0));
        assert_eq!(stats.reply_dropped, 0);
        assert_eq!((quality.underruns, quality.arrival_gap_max_ms), (0, 20));
    }
}

/// T1: the model talks on for 200 ms after the user starts, then yields. The
/// barge-in fires `BARGE_QUIET` after its last audible frame, not before: it
/// cuts the device, drops what was still queued, and the replacement plays.
#[tokio::test(start_paused = true)]
async fn voice_user_speech_cuts_and_flushes_once_the_model_has_been_quiet() {
    let quiet = usize::try_from(BARGE_QUIET.as_millis() / FRAME.as_millis()).unwrap();
    for signal in 0..3 {
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
        send(&tx, [said("Old answer")]).await;
        stream(&mut work, &tx, 25, 123).await;
        send(&tx, [heard(signal)]).await;
        stream(&mut work, &tx, 10, 123).await;
        // Silence from the model, one frame short of the quiet stretch.
        stream(&mut work, &tx, quiet - 2, 0).await;
        assert_eq!(cuts(&observer), 0);
        // A burst the pacer still holds when the stretch completes.
        send(&tx, (0..20).map(|_| MediaEvent::Audio(Box::new([0; 320])))).await;
        tick(work.as_mut(), 1).await;
        assert_eq!(cuts(&observer), 0);
        let before = frames(&observer).len();
        tick(work.as_mut(), 1).await;
        assert_eq!(cuts(&observer), 1);
        observer.finish(Ok(()));
        tick(work.as_mut(), 30).await;
        // Nothing of the old reply was written after the cut.
        assert_eq!(frames(&observer).len(), before);
        let written = frames(&observer).len();
        send(&tx, [said("New answer")]).await;
        send(&tx, audible(3)).await;
        send(&tx, [done()]).await;
        tick(work.as_mut(), 20).await;
        assert_eq!(frames(&observer).len(), written + 3);
        assert_eq!(observer.commands().last(), Some(&Command::Stop));
        observer.finish(Ok(()));
        let stats = work.await.unwrap();
        let trigger = if signal == 0 {
            BargeTrigger::SpeechStarted
        } else {
            BargeTrigger::UserTranscript
        };
        let quality = stats.quality;
        assert_eq!(
            (cuts(&observer), quality.cuts, quality.cut_trigger),
            (1, 1, Some(trigger))
        );
        assert_eq!(stats.reply_received, 35 + quiet as u64 - 2 + 20 + 3);
        assert_eq!(stats.reply_sent, (written + 3) as u64);
        // The retired stretch is neither an arrival gap nor an underrun.
        assert_eq!((quality.underruns, quality.arrival_gap_max_ms), (0, 20));
    }
}

/// T1, the device's failed call: after a barge-in the model said nothing
/// more, and 30 s later hassmic's duplex failsafe ended the open replacement
/// playback. That `played` used to fail the run at `premature_played` and
/// close the Live call. Now the cut reply settles and the run closes in order.
#[tokio::test(start_paused = true)]
async fn voice_device_ending_an_unanswered_replacement_settles_the_run() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let mut played = 0;
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        options(),
        std::future::pending(),
        |notice| {
            played += usize::from(matches!(notice, Notice::ReplyPlayed));
            Ok(())
        },
    ));
    send(&tx, [said("Old answer")]).await;
    stream(&mut work, &tx, 25, 123).await;
    send(&tx, [heard(1)]).await;
    stream(&mut work, &tx, 25, 0).await;
    assert_eq!(cuts(&observer), 0);
    stream(&mut work, &tx, 1, 0).await;
    assert_eq!(cuts(&observer), 1);
    observer.finish(Ok(()));
    stream(&mut work, &tx, 5, 0).await;
    assert_eq!(command_count(&observer, true), 2);
    // The device ends the replacement playback on its own: no stop was sent.
    observer.finish(Ok(()));
    let stats = work.await.unwrap();
    assert_eq!(played, 1);
    assert_eq!(observer.commands().last(), Some(&Command::EndRun));
    assert_eq!((stats.quality.cuts, stats.reply_received), (1, 56));
}
