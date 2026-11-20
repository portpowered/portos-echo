//! T1, the device's barge-in: the owner talked over a long reply and it
//! played on, then the run died with no reply rows in the trace.
use super::{yield_cases::stream, *};
use crate::bridge_runtime::bridge_pacing::BARGE_WINDOW;
use std::cell::{Cell, RefCell};

const FRAME: Duration = Duration::from_millis(20);

/// The Echo's configuration: the run listens on after each reply.
fn persistent() -> Options {
    Options {
        listen_timeout: Some(Duration::from_secs(3)),
        ..options()
    }
}

fn cuts(observer: &Observer) -> usize {
    command_count(observer, false)
}

fn starts(observer: &Observer) -> usize {
    command_count(observer, true)
}

/// Keep each settled reply's stats and count the `reply.played` notices.
fn rows<'a>(
    settled: &'a RefCell<Vec<crate::bridge_voice::Stats>>,
    played: &'a Cell<usize>,
) -> impl FnMut(Notice) -> Result<(), Error> + 'a {
    move |notice| {
        match notice {
            Notice::ReplySettled(stats) => settled.borrow_mut().push(stats),
            Notice::ReplyPlayed => played.set(played.get() + 1),
            _ => (),
        }
        Ok(())
    }
}

/// The model streams a long reply faster than real time and the user talks
/// over it. The reply's audio stops reaching the device at once, the model's
/// yield cuts it, and the cut reply settles: its rows are written, nothing of
/// it plays later and no playback is opened for it again. The pinned V3
/// session has no cancel request; the bridge retires the call's receive side.
#[tokio::test(start_paused = true)]
async fn barge_in_stops_reply_audio_and_settles_the_cut_reply() {
    let quiet = usize::try_from(BARGE_QUIET.as_millis() / FRAME.as_millis()).unwrap();
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let settled = RefCell::new(Vec::new());
    let played = Cell::new(0);
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        persistent(),
        std::future::pending(),
        rows(&settled, &played),
    ));
    send(&tx, [said("A long answer")]).await;
    // Faster than real time: the bridge queues what the device is not owed yet.
    send(&tx, audible(30)).await;
    tick(work.as_mut(), 1).await;
    stream(&mut work, &tx, 25, 123).await;
    let before = frames(&observer).len();
    // The user talks over the reply while the model talks on for 300 ms.
    send(&tx, [heard(1)]).await;
    stream(&mut work, &tx, 15, 123).await;
    let stopped = frames(&observer).len();
    assert!(
        stopped <= before + 1,
        "{} reply frames reached the device after the barge-in",
        stopped - before
    );
    // The model yields. Its quiet stretch confirms the barge-in.
    stream(&mut work, &tx, quiet + 1, 0).await;
    assert_eq!((cuts(&observer), frames(&observer).len()), (1, stopped));
    observer.finish(Ok(()));
    tick(work.as_mut(), 5).await;
    // The cut reply settles with its rows; no playback is opened for it again.
    assert_eq!((settled.borrow().len(), played.get()), (1, 1));
    let quality = settled.borrow()[0].quality;
    assert_eq!(
        (quality.cuts, quality.cut_trigger),
        (1, Some(BargeTrigger::UserTranscript))
    );
    // Late audio of the cut response never plays, nor does audio that
    // follows the rest of the user's sentence.
    send(&tx, audible(10)).await;
    tick(work.as_mut(), 5).await;
    send(&tx, [heard(2)]).await;
    send(&tx, audible(10)).await;
    tick(work.as_mut(), 15).await;
    assert_eq!((starts(&observer), frames(&observer).len()), (1, stopped));
    // The model's answer to the interruption is a new reply.
    send(&tx, [said("New answer")]).await;
    send(&tx, audible(3)).await;
    send(&tx, [done()]).await;
    tick(work.as_mut(), 20).await;
    assert_eq!((starts(&observer), frames(&observer).len()), (2, stopped + 3));
    assert_eq!(observer.commands().last(), Some(&Command::Stop));
    observer.finish(Ok(()));
    let stats = work.await.unwrap();
    assert_eq!(observer.commands().last(), Some(&Command::EndRun));
    assert_eq!((settled.borrow().len(), played.get(), cuts(&observer)), (2, 2, 1));
    let answer = settled.borrow()[1].quality.cuts;
    assert_eq!((answer, port.retired, stats.reply_sent), (0, 1, (stopped + 3) as u64));
}

/// Noise or another voice with no yield from the model: the reply pauses
/// while the barge-in is pending and then plays whole. Nothing is cut,
/// flushed or lost.
#[tokio::test(start_paused = true)]
async fn barge_in_sign_under_a_talking_model_loses_no_reply_audio() {
    let talking = usize::try_from((BARGE_WINDOW + BARGE_QUIET * 2).as_millis() / FRAME.as_millis()).unwrap();
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let settled = RefCell::new(Vec::new());
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        persistent(),
        std::future::pending(),
        |notice| {
            if let Notice::ReplySettled(stats) = notice {
                settled.borrow_mut().push(stats);
            }
            Ok(())
        },
    ));
    send(&tx, [said("A long answer")]).await;
    stream(&mut work, &tx, 25, 123).await;
    send(&tx, [heard(1)]).await;
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
    let received = 25 + talking;
    assert_eq!((cuts(&observer), starts(&observer)), (0, 1));
    assert_eq!(frames(&observer).len(), received);
    assert_eq!(
        (stats.reply_received, stats.reply_sent, stats.reply_dropped),
        (received as u64, received as u64, 0)
    );
    let quality = settled.borrow()[0].quality;
    assert_eq!(
        (settled.borrow().len(), quality.cuts, quality.cut_trigger),
        (1, 0, None)
    );
    assert_eq!((quality.discarded_cut, quality.discarded_audible), (0, 0));
    assert_eq!(port.retired, 0);
}

/// The device's failed call: after the barge-in the user finished the
/// sentence and the model said nothing more. No playback is left open for the
/// device's failsafe to end; the run listens out its window from the user's
/// last word and closes in order.
#[tokio::test(start_paused = true)]
async fn barge_in_the_model_never_answers_ends_the_run_in_order() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let played = Cell::new(0);
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        persistent(),
        std::future::pending(),
        |notice| {
            played.set(played.get() + usize::from(matches!(notice, Notice::ReplyPlayed)));
            Ok(())
        },
    ));
    send(&tx, [said("A long answer")]).await;
    stream(&mut work, &tx, 25, 123).await;
    send(&tx, [heard(1)]).await;
    stream(&mut work, &tx, 26, 0).await;
    assert_eq!(cuts(&observer), 1);
    observer.finish(Ok(()));
    tick(work.as_mut(), 100).await;
    // Two seconds into the listen window the user's sentence ends: the
    // window runs again from there, and its done opens no reply.
    send(&tx, [heard(2), done()]).await;
    tick(work.as_mut(), 100).await;
    assert!(!observer.commands().contains(&Command::EndRun));
    let stats = work.await.unwrap();
    assert_eq!(observer.commands().last(), Some(&Command::EndRun));
    assert_eq!((starts(&observer), cuts(&observer), played.get()), (1, 1, 1));
    assert_eq!(
        (stats.quality.cuts, stats.reply_sent),
        (0, frames(&observer).len() as u64)
    );
}
