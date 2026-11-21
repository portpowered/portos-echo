//! T1: lip sync from the production hand-off (`Mouth::wrote`) on a paused
//! clock. The screen is a recording sink: taking, stalled, busy or absent.
use super::{LipSink, LipStats, Lips, cut, scope, settle};
use crate::{bridge_runtime::bridge_pacing::LeadPacer, bridge_voice::voice_source::Mouth};
use echo_audio::FrameQueue;
use echo_screen_contract::mouth::MouthBatch;
use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};
use tokio::time::{Duration, Instant, advance};

type Sent = Arc<Mutex<Vec<MouthBatch>>>;

/// A screen that asked for lip sync (or not) and takes batches (or not).
struct Screen {
    wanted: Option<bool>,
    takes: bool,
    sent: Sent,
}

impl LipSink for Screen {
    fn wanted(&self) -> Option<bool> {
        self.wanted
    }

    fn send(&self, batch: &MouthBatch) -> usize {
        self.sent.lock().unwrap().push(batch.clone());
        usize::from(self.takes)
    }
}

fn lips(wanted: Option<bool>, takes: bool, delay_ms: u64) -> (Lips, Sent) {
    let sent = Sent::default();
    let screen = Screen {
        wanted,
        takes,
        sent: sent.clone(),
    };
    (Lips::new(Box::new(screen), Duration::from_millis(delay_ms)), sent)
}

/// 20 ms of a 444 Hz square wave at `level`, or silence at 0.
fn frame(level: i16) -> Vec<u8> {
    (0..480)
        .flat_map(|sample| if (sample / 27) % 2 == 0 { level } else { -level }.to_le_bytes())
        .collect()
}

/// (sent, [(due, open)]) of every batch, in milliseconds on the lips' clock.
fn timeline(sent: &Sent) -> Vec<(u64, Vec<(u64, u8)>)> {
    let sent = sent.lock().unwrap();
    sent.iter()
        .map(|batch| {
            let samples = batch.samples.iter().map(|sample| (sample.at_ms, sample.open));
            (batch.sent_ms, samples.collect())
        })
        .collect()
}

/// Hand `levels` to the Echo one frame every 20 ms, as the pacer does.
async fn speak(mouth: &mut Mouth, levels: &[i16]) {
    for level in levels {
        mouth.wrote(&frame(*level), Instant::now());
        advance(Duration::from_millis(20)).await;
    }
}

#[tokio::test(start_paused = true)]
async fn lip_samples_are_stamped_hand_off_plus_the_configured_delay() {
    let (lips, sent) = lips(Some(true), true, 200);
    let stats = scope(Some(lips), async {
        advance(Duration::from_secs(1)).await;
        let mut mouth = Mouth::default();
        // Silence before the voice sends nothing.
        speak(&mut mouth, &[0; 5]).await;
        assert_eq!(timeline(&sent), []);
        // Seven loud frames from 1100 ms, then the voice stops.
        speak(&mut mouth, &[8000; 7]).await;
        speak(&mut mouth, &[0; 6]).await;
        settle(Instant::now())
    })
    .await;
    // Each frame is heard 200 ms after its hand-off. Five samples leave with
    // the fifth frame; the sample that shuts the mouth leaves at once, and
    // the silence after it sends nothing.
    assert_eq!(
        timeline(&sent),
        [
            (1180, vec![(1300, 70), (1320, 91), (1340, 98), (1360, 100), (1380, 100)]),
            (1280, vec![(1400, 100), (1420, 100), (1440, 60), (1460, 20), (1480, 0)]),
        ]
    );
    let expected = LipStats {
        batches: 2,
        samples: 10,
        dropped: 0,
        publish_max_ms: 80,
        lead_min_ms: 120,
        late: 0,
    };
    assert_eq!(stats, expected);
}

#[tokio::test(start_paused = true)]
async fn a_burst_is_stamped_by_its_place_in_the_echo_queue_and_the_delay_is_configurable() {
    for (delay, size) in [(200, 5), (120, 3), (0, 1), (1000, 5)] {
        let (lips, sent) = lips(Some(true), true, delay);
        let stats = scope(Some(lips), async {
            advance(Duration::from_millis(500)).await;
            let mut mouth = Mouth::default();
            // The pacer's opening credit: ten frames in one instant.
            for _ in 0..10 {
                mouth.wrote(&frame(8000), Instant::now());
            }
            // The reply ends mid-batch: settling sends what is pending.
            mouth.wrote(&frame(8000), Instant::now());
            settle(Instant::now())
        })
        .await;
        let timeline = timeline(&sent);
        let due: Vec<u64> = timeline.iter().flat_map(|(_, samples)| samples).map(|s| s.0).collect();
        let expected: Vec<u64> = (0..11).map(|index| 500 + index * 20 + delay).collect();
        assert_eq!(due, expected, "delay {delay}");
        assert!(
            timeline
                .iter()
                .all(|(sent, samples)| *sent == 500 && samples.len() <= size)
        );
        assert_eq!(timeline[0].1.len(), size);
        assert_eq!((stats.samples, stats.publish_max_ms), (11, 0));
        assert_eq!(
            (stats.lead_min_ms, stats.late),
            (u32::try_from(delay).unwrap(), u32::from(delay == 0))
        );
    }
}

#[tokio::test(start_paused = true)]
async fn samples_already_heard_when_the_reply_settles_are_dropped_not_sent_late() {
    let (lips, sent) = lips(Some(true), true, 200);
    let stats = scope(Some(lips), async {
        let mut mouth = Mouth::default();
        speak(&mut mouth, &[8000; 7]).await;
        // The reply settles after its last frame has played.
        advance(Duration::from_millis(400)).await;
        settle(Instant::now())
    })
    .await;
    assert_eq!(timeline(&sent).len(), 1);
    assert_eq!((stats.batches, stats.samples, stats.late), (1, 5, 0));
}

#[tokio::test(start_paused = true)]
async fn a_cut_drops_what_is_pending_and_shuts_the_mouth_now() {
    let (lips, sent) = lips(Some(true), true, 200);
    let stats = scope(Some(lips), async {
        advance(Duration::from_secs(1)).await;
        let mut mouth = Mouth::default();
        // Five samples leave; two more are pending when the reply is cut.
        speak(&mut mouth, &[8000; 7]).await;
        cut(Instant::now());
        settle(Instant::now())
    })
    .await;
    let timeline = timeline(&sent);
    assert_eq!(timeline.len(), 2);
    assert_eq!(timeline[0].1.len(), 5);
    // The shut sample is due at the cut, before samples the page still holds.
    assert_eq!(timeline[1], (1140, vec![(1140, 0)]));
    assert_eq!((stats.batches, stats.samples, stats.late), (2, 6, 0));
}

#[tokio::test(start_paused = true)]
async fn samples_left_by_a_window_that_never_settled_do_not_leave_with_the_next_reply() {
    let (lips, sent) = lips(Some(true), true, 200);
    scope(Some(lips), async {
        let mut mouth = Mouth::default();
        // Two samples stay pending: the reply ends without a settle.
        speak(&mut mouth, &[8000; 2]).await;
        advance(Duration::from_secs(60)).await;
        let mut mouth = Mouth::default();
        speak(&mut mouth, &[8000; 5]).await;
        settle(Instant::now())
    })
    .await;
    let timeline = timeline(&sent);
    assert_eq!(timeline.len(), 1);
    assert!(timeline[0].1.iter().all(|(due, _)| *due >= 60_000), "{timeline:?}");
}

#[tokio::test(start_paused = true)]
async fn nothing_is_analysed_or_sent_for_a_screen_that_did_not_ask() {
    for wanted in [Some(false), None] {
        let (lips, sent) = lips(wanted, true, 200);
        let stats = scope(Some(lips), async {
            let mut mouth = Mouth::default();
            speak(&mut mouth, &[8000; 20]).await;
            settle(Instant::now())
        })
        .await;
        assert_eq!((timeline(&sent), stats), (vec![], LipStats::default()));
    }
    // Outside a scope the hand-off does nothing at all.
    let mut mouth = Mouth::default();
    speak(&mut mouth, &[8000; 20]).await;
    assert_eq!(settle(Instant::now()), LipStats::default());
}

/// The production pacer forwarding one reply: when each frame was handed to
/// the Echo, and when the mouth model says the reply's sound ends.
async fn handed(lips: Option<Lips>) -> (Vec<Duration>, Option<Duration>, LipStats) {
    scope(lips, async {
        let mut queue = FrameQueue::new(NonZeroUsize::new(64).unwrap());
        for index in 0..60 {
            queue.push(frame(if index % 20 < 15 { 8000 } else { 0 }));
        }
        let mut pacer = LeadPacer::new(200);
        let mut mouth = Mouth::default();
        let start = Instant::now();
        let mut times = Vec::new();
        while let Some(frame) = pacer.next_frame(&mut queue).await {
            let now = Instant::now();
            mouth.wrote(&frame, now);
            times.push(now - start);
        }
        mouth.settle();
        let mut end = None;
        while let Some(due) = mouth.due() {
            mouth.poll(due);
            end = Some(due);
        }
        (times, end.map(|end| end - start), settle(Instant::now()))
    })
    .await
}

#[tokio::test(start_paused = true)]
async fn audio_hand_off_timing_is_the_same_with_the_screen_taking_stalled_busy_or_absent() {
    let (alone, end, none) = handed(None).await;
    // The last audible frame is the 55th: it starts 1080 ms into the Echo's
    // queue and is heard 80 ms later, for 20 ms.
    assert_eq!(end, Some(Duration::from_millis(1180)));
    // Ten frames of opening credit, then one every 20 ms.
    assert_eq!(alone.len(), 60);
    assert_eq!(alone[9], Duration::ZERO);
    assert_eq!(alone[59], Duration::from_millis(1000));
    assert_eq!(none, LipStats::default());

    let (taking, sent) = lips(Some(true), true, 200);
    let (times, heard, taken) = handed(Some(taking)).await;
    assert_eq!((times, heard), (alone.clone(), end));
    assert_eq!((taken.dropped, taken.late), (0, 0));
    assert!(taken.batches >= 9 && taken.samples >= 45, "{taken:?}");
    assert_eq!(usize::try_from(taken.batches).unwrap(), sent.lock().unwrap().len());

    // A screen that is behind takes nothing: every batch is dropped, not kept.
    let (stalled, _) = lips(Some(true), false, 200);
    let (times, heard, dropped) = handed(Some(stalled)).await;
    assert_eq!((times, heard), (alone.clone(), end));
    assert_eq!(
        dropped,
        LipStats {
            dropped: taken.batches,
            ..taken
        }
    );

    for wanted in [Some(false), None] {
        let (absent, sent) = lips(wanted, false, 200);
        let (times, heard, stats) = handed(Some(absent)).await;
        assert_eq!((times, heard), (alone.clone(), end));
        assert_eq!((stats, sent.lock().unwrap().len()), (LipStats::default(), 0));
    }
}
