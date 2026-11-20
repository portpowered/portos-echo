use super::super::{ReplyMeter, ReplyQuality};
use super::*;
use std::{future::Future, num::NonZeroUsize, task::Poll};

/// Frames that only carry their order.
impl Pcm for u16 {
    fn audible(&self) -> bool {
        false
    }

    fn concealment(&self) -> Self {
        *self
    }

    fn fade_in(&mut self) {}
}

/// Arrival gaps in ms measured on the Echo's armv7 bridge, worst first:
/// the longest was 62 ms, 7 of 2064 exceeded 40 ms, 108 fell in 26..=40.
const JITTER: [u64; 20] = [
    62, 2, 4, 12, 36, 4, 20, 20, 38, 2, 20, 20, 45, 3, 12, 20, 28, 12, 20, 20,
];

/// Forward a jittered real-time stream and report the Echo's modelled queue.
async fn jittered(mut pacer: ClockPacer<u16>, frames: u16) -> ReplyQuality {
    let mut queue = FrameQueue::new(NonZeroUsize::new(128).unwrap());
    let mut meter = ReplyMeter::default();
    let mut arrival = Instant::now();
    for frame in 0..frames {
        arrival += Duration::from_millis(JITTER[usize::from(frame) % JITTER.len()]);
        // Forward everything due before the next frame arrives.
        while !queue.is_empty() {
            tokio::select! {
                biased;
                sent = pacer.next_frame(&mut queue, false) => { sent.unwrap(); meter.wrote(Instant::now()); }
                () = tokio::time::sleep_until(arrival) => break,
            }
        }
        tokio::time::sleep_until(arrival).await;
        meter.arrived(arrival);
        queue.push(frame);
    }
    while pacer.next_frame(&mut queue, true).await.is_some() {
        meter.wrote(Instant::now());
    }
    meter.settle(0)
}

#[tokio::test(start_paused = true)]
async fn bridge_pacing_measured_jitter_never_runs_the_primed_echo_dry() {
    assert_eq!(JITTER.iter().sum::<u64>(), 20 * 20);
    let quality = jittered(ClockPacer::new(200), 1000).await;
    assert_eq!((quality.arrival_gap_max_ms, quality.late_arrivals), (62, 99));
    assert_eq!((quality.underruns, quality.underrun_max_ms), (0, 0));
    assert!(quality.depth_min_ms >= 20, "{quality:?}");
    assert!(quality.depth_max_ms <= 200, "{quality:?}");
    // The regression this guards: the same stream forwarded frame by frame.
    let unprimed = ClockPacer {
        floor: Duration::ZERO,
        target: Duration::ZERO,
        ..ClockPacer::new(200)
    };
    let quality = jittered(unprimed, 1000).await;
    assert!(quality.underruns >= 40, "{quality:?}");
    assert_eq!(quality.depth_min_ms, 0);
}

#[tokio::test(start_paused = true)]
async fn bridge_pacing_prime_is_bounded_by_lead_and_survives_cancelled_polls() {
    let mut queue = FrameQueue::new(NonZeroUsize::new(30).unwrap());
    queue.push(0);
    queue.push(1);
    let mut pacer = ClockPacer::<u16>::new(60);
    let start = Instant::now();
    for _ in 0..3 {
        let mut next = Box::pin(pacer.next_frame(&mut queue, false));
        std::future::poll_fn(|cx| {
            assert!(next.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(next);
        tokio::time::advance(Duration::from_millis(10)).await;
    }
    assert_eq!(queue.len(), 2);
    // Two frames are under the 60 ms prime: held 60 ms from the first poll.
    assert_eq!(pacer.next_frame(&mut queue, false).await, Some(0));
    assert_eq!(Instant::now() - start, Duration::from_millis(60));
    assert_eq!(pacer.next_frame(&mut queue, false).await, Some(1));
    assert_eq!(Instant::now() - start, Duration::from_millis(60));
    // A dry device again: the end of the reply and a full prime never wait.
    tokio::time::advance(Duration::from_secs(1)).await;
    let late = Instant::now();
    queue.push(2);
    assert_eq!(pacer.next_frame(&mut queue, true).await, Some(2));
    tokio::time::advance(Duration::from_secs(1)).await;
    for frame in 3..6 {
        queue.push(frame);
    }
    assert_eq!(pacer.next_frame(&mut queue, false).await, Some(3));
    assert_eq!(Instant::now() - late, Duration::from_secs(1));
    // A smooth call keeps that lead-bounded prime; measured lateness lifts it.
    assert_eq!(pacer.retarget(PRIME), Duration::from_millis(60));
    assert_eq!(pacer.retarget(PRIME * 2), Duration::from_millis(200));
}

#[tokio::test(start_paused = true)]
async fn bridge_pacing_stepped_sim_clock_never_holds_a_frame() {
    // The M4 sim releases five frames, then waits for them to play
    // before it steps the bridge clock again.
    let clock = crate::bridge_mcp::bridge_clock::receipt::Context::for_voice_test();
    let mut pacer = clock.owning(async { ClockPacer::new(200) }).await;
    let mut queue = FrameQueue::new(NonZeroUsize::new(4).unwrap());
    // Lateness is never covered there, and a stalled reply gets no fade-out.
    assert_eq!(pacer.retarget(PRIME * 3), Duration::ZERO);
    queue.push(pcm(SPEECH));
    let start = Instant::now();
    assert_eq!(pacer.next_frame(&mut queue, false).await, Some(pcm(SPEECH)));
    assert_eq!(Instant::now(), start);
    assert!(!pacer.conceals(false));
    assert_eq!(pacer.fade_out(false).await, None);
    assert_eq!(pacer.take_playout(), Playout::default());
}

const SPEECH: i16 = 8000;

fn pcm(level: i16) -> Vec<u8> {
    level.to_le_bytes().repeat(480)
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
    frames: Vec<(u128, Vec<u8>)>,
}

impl Heard {
    fn written(&self) -> Vec<i16> {
        self.frames.iter().map(|(_, frame)| samples(frame)[240]).collect()
    }
}

/// As the bridge's playback loop asks: a queued frame, or else the fade-out.
async fn next(pacer: &mut ClockPacer<Vec<u8>>, queue: &mut FrameQueue<Vec<u8>>) -> Option<Vec<u8>> {
    if queue.is_empty() {
        pacer.fade_out(false).await
    } else {
        pacer.next_frame(queue, false).await
    }
}

/// Play one reply: each `(gap_ms, level)` is a frame the call delivers that
/// long after the one before. The meter outlives the reply, as in a call.
async fn reply(meter: &mut ReplyMeter, mut pacer: ClockPacer<Vec<u8>>, script: &[(u64, i16)]) -> Heard {
    let mut queue = FrameQueue::new(NonZeroUsize::new(128).unwrap());
    let mut frames = Vec::new();
    let mut first = None;
    let mut wrote = |meter: &mut ReplyMeter, frame: Vec<u8>| {
        let now = Instant::now();
        meter.wrote(now);
        frames.push(((now - *first.get_or_insert(now)).as_millis(), frame));
    };
    let mut arrival = Instant::now();
    for (gap, level) in script {
        arrival += Duration::from_millis(*gap);
        while !queue.is_empty() || pacer.conceals(false) {
            tokio::select! {
                biased;
                sent = next(&mut pacer, &mut queue) => if let Some(frame) = sent { wrote(meter, frame); },
                () = tokio::time::sleep_until(arrival) => break,
            }
        }
        tokio::time::sleep_until(arrival).await;
        let wanted = meter.arrived(arrival);
        meter.targeted(pacer.retarget(wanted));
        queue.push(pcm(*level));
    }
    while let Some(frame) = pacer.next_frame(&mut queue, true).await {
        wrote(meter, frame);
    }
    meter.paced(pacer.take_playout());
    Heard {
        quality: meter.settle(0),
        frames,
    }
}

/// The pacer before the adaptive playout: a fixed 100 ms prime and no more.
fn fixed() -> ClockPacer<Vec<u8>> {
    ClockPacer {
        adaptive: false,
        ..ClockPacer::new(200)
    }
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
    let heard = reply(&mut ReplyMeter::default(), ClockPacer::new(200), &script).await;
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
    // The fixed prime it replaces does exactly the same on a smooth call.
    let before = reply(&mut ReplyMeter::default(), fixed(), &script).await;
    assert_eq!(before.frames, heard.frames);
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
    // Before: the Echo plays its 100 ms, cuts off mid-wave, and stays dry
    // 170 ms; the burst then rides on as 260 ms of delay to the reply's end.
    let before = reply(&mut ReplyMeter::default(), fixed(), &script).await.quality;
    assert_eq!((before.underruns, before.underrun_max_ms), (1, 170));
    assert_eq!((before.arrival_gap_max_ms, before.depth_max_ms), (270, 180));
    assert_eq!(playout(&before), [1, 0, 0, 0]);

    let mut meter = ReplyMeter::default();
    let heard = reply(&mut meter, ClockPacer::new(200), &script).await;
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
    let before = reply(&mut ReplyMeter::default(), ClockPacer::new(200), &script)
        .await
        .quality;
    assert_eq!((before.underruns, before.reprimes), (1, 1));
    let heard = reply(&mut ReplyMeter::call(), ClockPacer::new(200), &script).await;
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
    // Before: every reply starts on the same 100 ms and runs dry again.
    let mut meter = ReplyMeter::default();
    let first = reply(&mut meter, fixed(), &script).await.quality;
    let second = reply(&mut meter, fixed(), &script).await.quality;
    assert_eq!((first.underruns, second.underruns), (1, 1));
    assert_eq!((second.underrun_max_ms, target(&second)), (170, [100, 100, 100]));

    let mut meter = ReplyMeter::default();
    let first = reply(&mut meter, ClockPacer::new(200), &script).await.quality;
    assert_eq!((first.underruns, playout(&first)), (1, [1, 20, 1, 0]));
    // The call has shown a 270 ms stall: the next reply starts on 300 ms,
    // 200 ms later than a smooth call's would, and the same stall passes
    // with 30 ms to spare and nothing to conceal.
    let heard = reply(&mut meter, ClockPacer::new(200), &script).await;
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
    let mut meter = ReplyMeter::default();
    let first = reply(&mut meter, ClockPacer::new(200), &script).await.quality;
    assert_eq!(first.underruns, 1);
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_secs(45)).await;
        let later = reply(&mut meter, ClockPacer::new(200), &script).await.quality;
        assert_eq!((later.underruns, later.reprimes), (0, 0), "{later:?}");
        assert_eq!(target(&later), [300, 300, 300]);
    }
}

#[tokio::test(start_paused = true)]
async fn reply_playout_conceals_only_an_unfinished_reply_cut_mid_speech() {
    let mut queue = FrameQueue::new(NonZeroUsize::new(8).unwrap());
    let mut pacer = ClockPacer::new(200);
    let start = Instant::now();
    // A reply that pauses in silence has nothing to fade.
    queue.push(pcm(SPEECH));
    queue.push(pcm(0));
    assert_eq!(pacer.next_frame(&mut queue, true).await, Some(pcm(SPEECH)));
    assert!(pacer.conceals(false));
    assert_eq!(pacer.next_frame(&mut queue, true).await, Some(pcm(0)));
    assert!(!pacer.conceals(false));
    assert_eq!(pacer.fade_out(false).await, None);
    // Nor has a finished one: its last frame is the end of the reply.
    queue.push(pcm(SPEECH));
    assert_eq!(pacer.next_frame(&mut queue, true).await, Some(pcm(SPEECH)));
    assert!(pacer.conceals(false) && !pacer.conceals(true));
    assert_eq!(pacer.fade_out(true).await, None);
    assert_eq!(Instant::now(), start);
    // Unfinished and mid-speech: a cancelled wait gives nothing away, and
    // the fade-out comes when the three frames sent have 20 ms left.
    assert_eq!(pacer.next_frame(&mut queue, false).await, None);
    let mut next = Box::pin(pacer.fade_out(false));
    std::future::poll_fn(|cx| {
        assert!(next.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(next);
    assert!(pacer.conceals(false));
    let faded = pacer.fade_out(false).await.unwrap();
    assert_eq!(Instant::now() - start, Duration::from_millis(40));
    assert_eq!(faded, pcm(SPEECH).concealment());
    assert!(!pacer.conceals(false));
    // A frame that comes before the Echo is dry resumes without a restart.
    queue.push(pcm(SPEECH));
    let mut resumed = pcm(SPEECH);
    resumed.fade_in();
    assert_eq!(pacer.next_frame(&mut queue, false).await, Some(resumed));
    assert_eq!(Instant::now() - start, Duration::from_millis(40));
    let playout = pacer.take_playout();
    assert_eq!((playout.reprimes, playout.concealed, playout.fade_ins), (0, 1, 1));
    assert_eq!(pacer.take_playout(), Playout::default());
}

#[test]
fn reply_playout_fades_are_continuous_with_the_audio_around_them() {
    let ramp: Vec<u8> = (0..480_i16).flat_map(|sample| (sample * 50).to_le_bytes()).collect();
    assert!(ramp.audible() && !pcm(64).audible() && pcm(-65).audible());
    // The fade-out starts on the frame's last sample and ends in silence.
    let faded = samples(&ramp.concealment());
    assert_eq!((faded[0], faded[1], faded[240], faded[479]), (23950, 23850, 5975, 0));
    assert_eq!(samples(&pcm(-SPEECH).concealment())[479], -16);
    // The fade-in starts from silence and reaches the frame's own level.
    let mut risen = pcm(SPEECH);
    risen.fade_in();
    let risen = samples(&risen);
    assert_eq!((risen[0], risen[1], risen[240], risen[479]), (0, 16, 4000, 7983));
    assert!(pcm(0).concealment() == pcm(0));
}
