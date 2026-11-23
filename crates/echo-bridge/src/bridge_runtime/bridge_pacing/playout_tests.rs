use super::{ReplyMeter, ReplyQuality};
use crate::bridge_codex::ReplyAudio;
use echo_core_api::ReplyFrame;
use tokio::time::{Duration, Instant};

/// Arrival gaps in ms measured on the Echo's armv7 bridge, worst first:
/// the longest was 62 ms, 7 of 2064 exceeded 40 ms, 108 fell in 26..=40.
const JITTER: [u64; 20] = [
    62, 2, 4, 12, 36, 4, 20, 20, 38, 2, 20, 20, 45, 3, 12, 20, 28, 12, 20, 20,
];

#[tokio::test(start_paused = true)]
async fn reply_playout_measured_jitter_keeps_the_primed_queue_wet() {
    assert_eq!(JITTER.iter().sum::<u64>(), 20 * 20);
    let script: Vec<_> = JITTER.into_iter().cycle().take(1000).map(|gap| (gap, 0)).collect();
    let quality = reply(&mut paced(), &mut ReplyMeter::default(), &script).await.quality;
    assert_eq!((quality.arrival_gap_max_ms, quality.late_arrivals), (62, 99));
    assert_eq!((quality.underruns, quality.underrun_max_ms), (0, 0));
    assert!(quality.depth_min_ms >= 20, "{quality:?}");
    assert!(quality.depth_max_ms <= 200, "{quality:?}");
}

const SPEECH: i16 = 8000;

fn pcm(level: i16) -> ReplyFrame {
    let mut frame = [0; 960];
    frame.as_chunks_mut::<2>().0.fill(level.to_le_bytes());
    frame
}

fn samples(frame: &[u8]) -> Vec<i16> {
    let (samples, _) = frame.as_chunks::<2>();
    samples.iter().map(|sample| i16::from_le_bytes(*sample)).collect()
}

/// Two seconds of talk: 1.6 s of speech, then a 400 ms pause.
fn talk(frames: usize) -> Vec<(u64, i16)> {
    (0..frames)
        .map(|frame| (20, if frame % 100 < 80 { SPEECH } else { 0 }))
        .collect()
}

/// A 270 ms network stall, then the 13 frames it delayed in one burst.
fn stall() -> Vec<(u64, i16)> {
    let mut script = vec![(270, SPEECH)];
    script.extend([(0, SPEECH); 12]);
    script
}

struct Heard {
    quality: ReplyQuality,
    /// Each frame written to the Echo, and when, in ms from the first write.
    frames: Vec<(u128, ReplyFrame)>,
}

impl Heard {
    fn written(&self) -> Vec<i16> {
        self.frames.iter().map(|(_, frame)| samples(frame)[240]).collect()
    }
}

/// Reuse the production adapter, keeping the World and arrival history
/// alive when simulating successive replies.
async fn reply(pacer: &mut ReplyAudio, meter: &mut ReplyMeter, script: &[(u64, i16)]) -> Heard {
    let mut frames = Vec::new();
    let mut first = None;
    let mut wrote = |pacer: &ReplyAudio, meter: &mut ReplyMeter, frame: ReplyFrame| {
        let now = Instant::now();
        pacer.handed();
        meter.wrote(now);
        frames.push(((now - *first.get_or_insert(now)).as_millis(), frame));
    };
    let mut arrival = Instant::now();
    for (gap, level) in script {
        arrival += Duration::from_millis(*gap);
        while pacer.has_frames() || pacer.stalled() {
            tokio::select! {
                biased;
                sent = pacer.stage(true) => if let Some(frame) = sent { wrote(pacer, meter, frame); },
                () = tokio::time::sleep_until(arrival) => break,
            }
        }
        tokio::time::sleep_until(arrival).await;
        meter.arrived(arrival);
        meter.targeted(pacer.arrived(*level != 0));
        pacer.push_pcm(&pcm(*level));
    }
    pacer.finish().unwrap();
    while let Some(frame) = pacer.stage(false).await {
        wrote(pacer, meter, frame);
    }
    meter.paced(pacer.settle().playout);
    pacer.reset(false).unwrap();
    Heard {
        quality: meter.settle(0),
        frames,
    }
}

fn paced() -> ReplyAudio {
    ReplyAudio::new(200).unwrap()
}

fn playout(quality: &ReplyQuality) -> [u32; 4] {
    [
        quality.reprimes,
        quality.concealment_ms,
        quality.fade_ins,
        quality.drained_frames,
    ]
}

fn target(quality: &ReplyQuality) -> [u32; 3] {
    [quality.target_min_ms, quality.target_max_ms, quality.target_final_ms]
}

#[tokio::test(start_paused = true)]
async fn reply_playout_steady_arrivals_stay_at_the_floor_untouched() {
    let script = talk(500);
    let heard = reply(&mut paced(), &mut ReplyMeter::default(), &script).await;
    let quality = heard.quality;
    assert_eq!((quality.underruns, quality.arrival_gap_max_ms), (0, 20));
    // The five frames of the prime stack up to 80 ms; each later frame finds 80.
    assert_eq!((quality.depth_min_ms, quality.depth_max_ms), (20, 80));
    assert_eq!(target(&quality), [100, 100, 100]);
    assert_eq!(playout(&quality), [0, 0, 0, 0]);
    // Every frame goes out as it came: 100 ms gathers, then one per arrival.
    let sent: Vec<i16> = script.iter().map(|(_, level)| *level).collect();
    assert_eq!(heard.written(), sent);
    assert!(
        heard
            .frames
            .iter()
            .all(|(_, frame)| *frame == pcm(SPEECH) || *frame == pcm(0))
    );
    let times: Vec<u128> = heard.frames.iter().map(|(at, _)| *at).collect();
    assert_eq!(times[..7], [0, 0, 0, 0, 0, 20, 40]);
    assert!(times[5..].iter().copied().eq((1..=495).map(|frame| 20 * frame)));
}

/// 2 s of talk, a stall in mid-speech, then 40 s more.
fn stalled() -> Vec<(u64, i16)> {
    let mut script = talk(50);
    script.extend(stall());
    script.extend(talk(2000));
    script
}

#[tokio::test(start_paused = true)]
async fn reply_playout_first_stall_fades_out_reprimes_and_fades_in() {
    let script = stalled();
    let mut pacer = paced();
    let mut meter = ReplyMeter::default();
    let heard = reply(&mut pacer, &mut meter, &script).await;
    let quality = heard.quality;
    // One dry spell still (nothing foretold the stall), but it starts after
    // a 20 ms fade-out and lasts 190 ms: the restart waits two frames past
    // the burst for the 300 ms the stall now asks for.
    assert_eq!((quality.underruns, quality.underrun_max_ms), (1, 190));
    assert_eq!(playout(&quality), [1, 20, 1, 0]);
    assert_eq!(target(&quality), [100, 300, 300]);
    // The 50th frame went out at 900 ms; 80 ms later the Echo has 20 ms left
    // and gets that frame mirrored and faded to silence.
    let (at, faded) = &heard.frames[50];
    assert_eq!((heard.frames[49].0, *at), (900, 980));
    let faded = samples(faded);
    assert_eq!((faded[0], faded[240], faded[479]), (8000, 4000, 16));
    // The burst and the two frames after it restart playout, faded in.
    let (at, resumed) = &heard.frames[51];
    assert_eq!(*at, 1210);
    let resumed = samples(resumed);
    assert_eq!((resumed[0], resumed[240], resumed[479]), (0, 4000, 7983));
    assert_eq!(heard.frames[52], (1210, pcm(SPEECH)));
    // Nothing else is touched, and the 300 ms is kept to the reply's end:
    // 2063 accepted, 1 added.
    assert_eq!((quality.depth_max_ms, heard.frames.len()), (180, 2063 + 1));
    let altered = heard.frames.iter().filter(|(_, f)| *f != pcm(SPEECH) && *f != pcm(0));
    assert_eq!(altered.count(), 2);
}

/// The device's first reply: it started on the 100 ms floor and ran dry at
/// the call's first stall, which every call measured has in its first reply.
#[tokio::test(start_paused = true)]
async fn reply_playout_first_reply_of_a_call_rides_out_a_stall_in_its_first_second() {
    let mut script = talk(30);
    script.extend(stall());
    script.extend(talk(100));
    let before = reply(&mut paced(), &mut ReplyMeter::default(), &script).await.quality;
    assert_eq!((before.underruns, before.reprimes), (1, 1));
    let mut pacer = paced();
    pacer.expect_jitter();
    let heard = reply(&mut pacer, &mut ReplyMeter::default(), &script).await;
    let quality = heard.quality;
    assert_eq!((quality.underruns, quality.reprimes, quality.concealment_ms), (0, 0, 0));
    assert_eq!(target(&quality), [300, 300, 300]);
    let sent: Vec<i16> = script.iter().map(|(_, level)| *level).collect();
    assert_eq!(heard.written(), sent);
}

#[tokio::test(start_paused = true)]
async fn reply_playout_next_reply_rides_out_the_stall_the_call_has_shown() {
    let mut script = talk(50);
    script.extend(stall());
    script.extend(talk(100));
    let mut pacer = paced();
    let mut meter = ReplyMeter::default();
    let first = reply(&mut pacer, &mut meter, &script).await.quality;
    assert_eq!((first.underruns, playout(&first)), (1, [1, 20, 1, 0]));
    // The call has shown a 270 ms stall: the next reply starts on 300 ms,
    // 200 ms later than a smooth call's would, and the same stall passes
    // with 30 ms to spare and nothing to conceal.
    let heard = reply(&mut pacer, &mut meter, &script).await;
    let second = heard.quality;
    assert_eq!((second.underruns, second.arrival_gap_max_ms), (0, 270));
    assert_eq!((target(&second), playout(&second)), ([300, 300, 300], [0, 0, 0, 0]));
    assert_eq!(heard.frames.len(), script.len());
    assert!(
        heard
            .frames
            .iter()
            .all(|(_, frame)| *frame == pcm(SPEECH) || *frame == pcm(0))
    );
}

/// The device's choppy replies: the call stalls again in a reply 45 s after
/// the first stall. The cushion that stall asked for had decayed by then, so
/// the Echo ran dry again, in four replies of one call.
#[tokio::test(start_paused = true)]
async fn reply_playout_keeps_the_cushion_between_stalls_of_one_call() {
    let mut script = talk(50);
    script.extend(stall());
    script.extend(talk(100));
    let mut pacer = paced();
    let mut meter = ReplyMeter::default();
    let first = reply(&mut pacer, &mut meter, &script).await.quality;
    assert_eq!(first.underruns, 1);
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_secs(45)).await;
        let later = reply(&mut pacer, &mut meter, &script).await.quality;
        assert_eq!((later.underruns, later.reprimes), (0, 0), "{later:?}");
        assert_eq!(target(&later), [300, 300, 300]);
    }
}
