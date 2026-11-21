//! T3: the first second of a reply. On the Echo the first reply of a call
//! ran dry at its first network stall, and a short answer was held by the
//! transcript of the question and then dropped whole.
use super::bridge_barge::said;
use super::bridge_barge_mic::{Room, call};
use super::*;
use echo_bridge::bridge_codex::{Event, MediaEvent};
use tokio::sync::mpsc;

const FRAME: Duration = Duration::from_millis(20);

/// Frame `index` of a reply: its level names it.
fn level(index: usize) -> i16 {
    2000 + 10 * i16::try_from(index).unwrap()
}

async fn speak(model: &mpsc::Sender<MediaEvent>, frames: std::ops::Range<usize>, gap: Duration) {
    for index in frames {
        model
            .send(MediaEvent::Audio(Box::new([level(index); 320])))
            .await
            .unwrap();
        tokio::time::sleep(gap).await;
    }
}

async fn done(model: &mpsc::Sender<MediaEvent>, text: &str) {
    let text = text.into();
    model
        .send(MediaEvent::Control(Event::ReplyDone { text }))
        .await
        .unwrap();
}

/// What the device was sent of the reply, as frame indexes.
fn played(room: &Room) -> Vec<usize> {
    let played = room.played.lock().unwrap();
    let audible = played.iter().filter(|(_, level)| *level >= 1000);
    audible
        .map(|(_, level)| usize::from((*level + 5 - 2000) / 10))
        .collect()
}

/// The count of frames played when none is missing, repeated or out of
/// order from the first on. The converter smears each frame's edge into its
/// neighbour, so a chunk may carry the next frame's level.
fn whole(played: &[usize]) -> Result<usize, &[usize]> {
    let rising = played
        .windows(2)
        .all(|pair| pair[1] == pair[0] + 1 || pair[1] > pair[0] + 5);
    let tail = played.windows(2).filter(|pair| pair[1] > pair[0] + 5).count();
    if played.first().is_some_and(|first| *first <= 1) && rising && tail <= 1 {
        Ok(played.len())
    } else {
        Err(played)
    }
}

/// The call's first reply: 400 ms in, the network stalls 270 ms and then
/// delivers the 13 frames it delayed in one burst.
async fn stalled(model: &mpsc::Sender<MediaEvent>, _: &Room) {
    model.send(said("assistant", "An answer", false)).await.unwrap();
    speak(model, 0..20, FRAME).await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    speak(model, 20..33, Duration::ZERO).await;
    speak(model, 33..100, FRAME).await;
    done(model, "An answer").await;
}

#[tokio::test]
#[ignore = "T3 real Wyoming client and loopback device"]
async fn reply_start_rides_out_a_stall_in_the_first_second_of_a_call() {
    let (room, stats, settled) = tokio::time::timeout(OBSERVATION_TIMEOUT, Box::pin(call(stalled)))
        .await
        .unwrap();
    let quality = *settled[0].quality;
    writeln!(
        std::io::stdout(),
        "START stall: underruns={} underrun_max_ms={} reprimes={} target_min_ms={}",
        quality.underruns,
        quality.underrun_max_ms,
        quality.reprimes,
        quality.target_min_ms
    )
    .unwrap();
    // The Echo never ran dry, and every frame went out once, in order.
    assert_eq!((quality.underruns, quality.reprimes, quality.concealment_ms), (0, 0, 0));
    assert_eq!((whole(&played(&room)), stats.reply_dropped), (Ok(100), 0));
}

/// A short answer. The session's transcript of the question comes as the
/// answer begins: its first fragment before any of it has sounded, the rest
/// 600 ms in, as on the Echo, where that held every reply for 2 s.
async fn answered(model: &mpsc::Sender<MediaEvent>, _: &Room) {
    model.send(said("assistant", "Sure.", false)).await.unwrap();
    model.send(said("user", "what time", false)).await.unwrap();
    speak(model, 0..30, FRAME).await;
    model.send(said("user", " is it", false)).await.unwrap();
    model.send(said("user", "what time is it", true)).await.unwrap();
    speak(model, 30..90, FRAME).await;
    done(model, "Sure.").await;
}

#[tokio::test]
#[ignore = "T3 real Wyoming client and loopback device"]
async fn reply_start_is_not_held_by_the_transcript_of_the_turn_it_answers() {
    let (room, stats, settled) = tokio::time::timeout(OBSERVATION_TIMEOUT, Box::pin(call(answered)))
        .await
        .unwrap();
    let quality = *settled[0].quality;
    let heard = played(&room);
    writeln!(
        std::io::stdout(),
        "START answer: played={} cuts={} holds={}",
        heard.len(),
        quality.cuts,
        quality.holds
    )
    .unwrap();
    // Nothing held the answer, and all of it was played.
    assert_eq!(
        (quality.cuts, quality.holds, *room.flushed.lock().unwrap()),
        (0, 0, None)
    );
    assert_eq!((whole(&heard), stats.reply_dropped), (Ok(90), 0));
}
