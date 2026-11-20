//! T2: the canceller on the bridge's own clocks, against a made-up voice in
//! a made-up room. Paused time; no file, device or network.
use super::*;
use echo_audio::echo_room::{echo, hiss, level_db, mix, voice};
use std::io::Write;

const STEP: Duration = Duration::from_millis(20);
/// The Echo's speaker-to-microphone delay as the bridge sees it (baselines).
const DELAY_MS: u16 = 320;
const DELAY: usize = 5120;
const SECOND: usize = 16_000;

/// A reply as it is written to the Echo, in 20 ms frames of 24 kHz bytes,
/// and what the Echo's microphone hears of it at 16 kHz: 320 ms late, half
/// as loud, through a small room with a quiet floor.
fn reply(seconds: usize) -> (Vec<Vec<u8>>, Vec<i16>) {
    let spoken = voice(31, seconds * 24_000, 3000.0);
    let written = AudioFormat {
        rate: 24_000,
        width: 2,
        channels: 1,
        extra: std::collections::BTreeMap::default(),
    };
    let mut resampler = PcmResampler::new(&written, 16_000).unwrap();
    let mut scratch = vec![0; resampler.output_capacity()];
    let mut played = Vec::new();
    let (frames, _) = spoken.as_chunks::<480>();
    for frame in frames {
        let progress = resampler.process(frame, &mut scratch).unwrap();
        played.extend_from_slice(&scratch[..progress.written]);
    }
    played.resize(seconds * SECOND, 0);
    let frames = frames
        .iter()
        .map(|frame| frame.iter().flat_map(|sample| sample.to_le_bytes()).collect());
    let microphone = mix(&echo(&played, DELAY, 0.5, 9), &hiss(7, played.len(), 12.0));
    (frames.collect(), microphone)
}

/// Play `frames` as the bridge does, ten ahead and then one each 20 ms,
/// while the microphone's frames arrive `burst` at a time. Returns what
/// went upstream.
async fn call(canceller: &mut Canceller, frames: &[Vec<u8>], microphone: &[i16], burst: usize) -> Vec<i16> {
    let mut uplink = Vec::new();
    let mut heard = microphone.as_chunks::<320>().0.iter();
    for step in 0..frames.len() {
        let ahead = if step == 0 { 0..10 } else { step + 9..step + 10 };
        for frame in frames.get(ahead).unwrap_or_default() {
            canceller.played(frame, Instant::now());
        }
        tokio::time::advance(STEP).await;
        if (step + 1) % burst == 0 {
            for near in heard.by_ref().take(burst) {
                let clean = canceller.cleaned(near, Instant::now()).unwrap();
                uplink.extend_from_slice(&clean);
            }
        }
    }
    uplink
}

fn erle(microphone: &[i16], uplink: &[i16], from: usize) -> f64 {
    level_db(&microphone[from..uplink.len()]) - level_db(&uplink[from..])
}

/// (e) The reply's echo is the only sound in the room. After two seconds
/// the uplink is at least 20 dB quieter than the microphone, and the
/// canceller reports the 320 ms it found between the two.
#[tokio::test(start_paused = true)]
async fn echo_cancel_takes_the_reply_out_of_the_uplink_on_the_bridge_clock() {
    let (frames, microphone) = reply(8);
    let mut canceller = Canceller::new(Some(240)).unwrap();
    let uplink = call(&mut canceller, &frames, &microphone, 1).await;
    let measured = erle(&microphone, &uplink, 2 * SECOND);
    let report = canceller.rest().unwrap();
    writeln!(
        std::io::stdout(),
        "bridge_echo_only erle_after_2s_db={measured:.1} microphone_db={:.1} uplink_db={:.1} {report:?}",
        level_db(&microphone[2 * SECOND..]),
        level_db(&uplink[2 * SECOND..]),
    )
    .unwrap();
    assert!(measured >= 20.0, "{measured}");
    assert!((DELAY_MS - 24..=DELAY_MS + 8).contains(&report.delay_ms), "{report:?}");
    assert!(report.converged && report.erle_db10 >= 200, "{report:?}");
    // The next reply's window starts empty and keeps the delay.
    let next = Cancelled {
        delay_ms: report.delay_ms,
        erle_db10: 0,
        converged: false,
    };
    assert_eq!(canceller.rest(), Some(next));
}

/// (e) The network delivers the microphone in bursts of five frames, 100 ms
/// at once. The frames keep their place on the microphone's own clock and
/// the echo is cancelled as well.
#[tokio::test(start_paused = true)]
async fn echo_cancel_keeps_its_place_when_the_microphone_arrives_in_bursts() {
    let (frames, microphone) = reply(8);
    let mut canceller = Canceller::new(Some(240)).unwrap();
    let uplink = call(&mut canceller, &frames, &microphone, 5).await;
    let measured = erle(&microphone, &uplink, 2 * SECOND);
    let report = canceller.rest().unwrap();
    writeln!(
        std::io::stdout(),
        "bridge_echo_bursts erle_after_2s_db={measured:.1} {report:?}"
    )
    .unwrap();
    assert!(measured >= 20.0, "{measured}");
    // The last frame of a burst arrives as it ends: the clock settles there.
    assert!((DELAY_MS - 24..=DELAY_MS + 8).contains(&report.delay_ms), "{report:?}");
    assert!(report.converged && report.erle_db10 >= 200, "{report:?}");
}

/// (d) Switched off, and before the first reply frame of a run, every
/// microphone frame passes byte for byte.
#[tokio::test(start_paused = true)]
async fn echo_cancel_off_or_before_a_reply_passes_the_microphone_untouched() {
    let (frames, microphone) = reply(2);
    let mut off = Canceller::new(None).unwrap();
    assert_eq!(call(&mut off, &frames, &microphone, 1).await, microphone);
    assert_eq!(off.rest(), None);
    // On: the request before the reply is not filtered either.
    let mut on = Canceller::new(Some(240)).unwrap();
    for near in microphone.as_chunks::<320>().0 {
        assert_eq!(on.cleaned(near, Instant::now()).unwrap(), *near);
        tokio::time::advance(STEP).await;
    }
    let idle = Cancelled {
        delay_ms: 0,
        erle_db10: 0,
        converged: false,
    };
    assert_eq!(on.rest(), Some(idle));
    let uplink = call(&mut on, &frames, &microphone, 1).await;
    assert_ne!(uplink, microphone);
}

/// A barge-in flushes the Echo: reply frames it had not begun to play are
/// never heard, and are not kept as far-end audio for the microphone.
#[tokio::test(start_paused = true)]
async fn echo_cancel_drops_the_reply_frames_a_cut_flushed() {
    let (frames, _) = reply(1);
    let mut canceller = Canceller::new(Some(240)).unwrap();
    let start = Instant::now();
    for frame in &frames[..10] {
        canceller.played(frame, start);
    }
    tokio::time::advance(Duration::from_millis(60)).await;
    canceller.cut(Instant::now());
    let state = canceller.state.as_mut().unwrap();
    // The resampler holds back a few milliseconds of the 200 written.
    let kept: Vec<_> = state.far.iter().map(|(at, _)| (*at - start).as_millis()).collect();
    assert_eq!(kept, [0, 10, 20, 30, 40, 50]);
    // The replacement plays from its own first frame, not behind the old queue.
    canceller.played(&frames[10], Instant::now());
    let state = canceller.state.as_ref().unwrap();
    assert_eq!(state.far.back().map(|(at, _)| (*at - start).as_millis()), Some(70));
    assert_eq!(state.playout, Some(start + Duration::from_millis(80)));
}
