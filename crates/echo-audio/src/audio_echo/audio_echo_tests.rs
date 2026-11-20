//! T2: the canceller against a made-up voice in a made-up room. No test
//! here reads a file, a device or the network.
use super::echo_room::{echo, hiss, level_db, likeness, mix, voice};
use super::*;
use std::io::Write;

const SECOND: usize = 16_000;
/// The far end as loud as a reply is written to the Echo, and a room floor.
const FAR_LEVEL: f64 = 3000.0;
const ROOM_LEVEL: f64 = 12.0;

/// Both streams through the canceller in step, as the bridge keeps them.
fn cancel(canceller: &mut EchoCanceller, far: &[i16], microphone: &[i16]) -> Vec<i16> {
    let mut output = Vec::with_capacity(microphone.len());
    let (far, near) = (far.as_chunks::<ECHO_FRAME>().0, microphone.as_chunks::<ECHO_FRAME>().0);
    for (far, near) in far.iter().zip(near) {
        let mut frame = *near;
        canceller.render(far).unwrap();
        canceller.capture(&mut frame).unwrap();
        output.extend_from_slice(&frame);
    }
    output
}

/// How much quieter the canceller made `range`, in dB.
fn erle(microphone: &[i16], output: &[i16], range: std::ops::Range<usize>) -> f64 {
    level_db(&microphone[range.clone()]) - level_db(&output[range])
}

/// The first whole half second from `start` on with at least 20 dB of
/// reduction, in ms after `start`.
fn settled_after(microphone: &[i16], output: &[i16], start: usize) -> Option<usize> {
    (start..microphone.len() - SECOND / 2)
        .step_by(SECOND / 100)
        .find(|at| erle(microphone, output, *at..*at + SECOND / 2) >= 20.0)
        .map(|at| (at - start) / 16)
}

/// (a) Echo only: after two seconds the reply's echo is at least 20 dB
/// quieter, and AEC3 has found the 240 ms the room put between the two.
#[test]
fn echo_only_is_cancelled_by_twenty_decibels_after_two_seconds() {
    let far = voice(11, 8 * SECOND, FAR_LEVEL);
    let microphone = mix(&echo(&far, 3840, 0.5, 5), &hiss(3, far.len(), ROOM_LEVEL));
    let mut canceller = EchoCanceller::new(200);
    let output = cancel(&mut canceller, &far, &microphone);
    let measured = erle(&microphone, &output, 2 * SECOND..8 * SECOND);
    let first = settled_after(&microphone, &output, 0);
    let report = canceller.report();
    writeln!(
        std::io::stdout(),
        "echo_only erle_after_2s_db={measured:.1} settled_after_ms={first:?} microphone_db={:.1} output_db={:.1} {report:?}",
        level_db(&microphone[2 * SECOND..]),
        level_db(&output[2 * SECOND..]),
    )
    .unwrap();
    assert!(measured >= 20.0, "{measured}");
    assert!(first.is_some_and(|ms| ms <= 2000), "{first:?}");
    let delay = report.delay_ms.unwrap();
    assert!((200..=248).contains(&delay), "{delay}");
    assert!(report.converged && report.converged_after_ms.unwrap() <= 2000);
    assert!(report.erle_db10 >= 150, "{}", report.erle_db10);
    assert_eq!(report.frames, 800);
    assert!(report.residual_max * 4 < report.echo_max, "{report:?}");
}

/// (a) The hint is only a hint: an echo 600 ms late, three times what the
/// canceller was told, is found and cancelled as well.
#[test]
fn echo_far_from_the_hint_is_found_and_cancelled() {
    let far = voice(12, 8 * SECOND, FAR_LEVEL);
    let microphone = mix(&echo(&far, 9600, 0.5, 6), &hiss(4, far.len(), ROOM_LEVEL));
    let mut canceller = EchoCanceller::new(200);
    let output = cancel(&mut canceller, &far, &microphone);
    let measured = erle(&microphone, &output, 3 * SECOND..8 * SECOND);
    let report = canceller.report();
    writeln!(
        std::io::stdout(),
        "echo_far_from_hint erle_after_3s_db={measured:.1} {report:?}"
    )
    .unwrap();
    assert!(measured >= 20.0, "{measured}");
    let delay = report.delay_ms.unwrap();
    assert!((560..=608).contains(&delay), "{delay}");
}

/// (b) The playback delay moves by 140 ms in mid-reply: the canceller finds
/// the echo again and is back to 20 dB within two seconds.
#[test]
fn delay_change_in_mid_stream_reconverges_within_two_seconds() {
    let far = voice(13, 12 * SECOND, FAR_LEVEL);
    let change = 6 * SECOND;
    let mut heard = echo(&far, 2560, 0.5, 7);
    heard[change..].copy_from_slice(&echo(&far, 4800, 0.5, 7)[change..]);
    let microphone = mix(&heard, &hiss(5, far.len(), ROOM_LEVEL));
    let mut canceller = EchoCanceller::new(200);
    let output = cancel(&mut canceller, &far[..change], &microphone[..change]);
    let before = canceller.report();
    let mut output = output;
    output.extend(cancel(&mut canceller, &far[change..], &microphone[change..]));
    let after = canceller.report();
    let steady = erle(&microphone, &output, 2 * SECOND..change);
    let lost = erle(&microphone, &output, change..change + SECOND / 4);
    let again = settled_after(&microphone, &output, change);
    let regained = erle(&microphone, &output, change + 2 * SECOND..12 * SECOND);
    writeln!(
        std::io::stdout(),
        "delay_change erle_before_db={steady:.1} erle_first_250ms_after_db={lost:.1} resettled_after_ms={again:?} erle_from_2s_after_db={regained:.1} delay_before_ms={:?} delay_after_ms={:?}",
        before.delay_ms, after.delay_ms,
    )
    .unwrap();
    assert!(steady >= 20.0, "{steady}");
    assert!(again.is_some_and(|ms| ms <= 2000), "{again:?}");
    assert!(regained >= 20.0, "{regained}");
    assert!((144..=176).contains(&before.delay_ms.unwrap()), "{before:?}");
    assert!((284..=316).contains(&after.delay_ms.unwrap()), "{after:?}");
}

/// The same voice with the far end silent: what the canceller's own
/// high-pass filter leaves of it, and so what double talk is compared with.
fn alone(near: &[i16]) -> Vec<i16> {
    cancel(&mut EchoCanceller::new(200), &vec![0; near.len()], near)
}

/// A reply with somebody speaking over it from 5 s to 8 s. Returns how alike
/// their voice is to the microphone and to the output, and by how many dB
/// the canceller changed its level.
fn double_talk(name: &str, echo_gain: f64, near_level: f64) -> (f64, f64, f64) {
    let far = voice(14, 10 * SECOND, FAR_LEVEL);
    let talk = 5 * SECOND..8 * SECOND;
    let mut near = vec![0; far.len()];
    near[talk.clone()].copy_from_slice(&voice(99, talk.len(), near_level));
    let heard = mix(&echo(&far, 3840, echo_gain, 8), &hiss(6, far.len(), ROOM_LEVEL));
    let microphone = mix(&heard, &near);
    let output = cancel(&mut EchoCanceller::new(200), &far, &microphone);
    let clean = alone(&near);
    let before = likeness(&near[talk.clone()], &microphone[talk.clone()], 0);
    let after = likeness(&clean[talk.clone()], &output[talk.clone()], 0);
    let level = level_db(&output[talk.clone()]) - level_db(&clean[talk.clone()]);
    let echo_before = erle(&microphone, &output, 2 * SECOND..5 * SECOND);
    let echo_after = erle(&microphone, &output, 8 * SECOND + SECOND / 2..10 * SECOND);
    writeln!(
        std::io::stdout(),
        "double_talk {name}: echo_over_near_db={:.1} near_likeness_at_microphone={before:.3} near_likeness_after={after:.3} near_level_change_db={level:.1} erle_before_db={echo_before:.1} erle_after_db={echo_after:.1}",
        level_db(&heard[talk.clone()]) - level_db(&near[talk]),
    )
    .unwrap();
    assert!(echo_before >= 30.0 && echo_after >= 30.0, "{echo_before} {echo_after}");
    (before, after, level)
}

/// (c) Double talk: somebody speaks over the reply, whose echo is 6 dB louder
/// at the microphone than they are. Their voice comes through alike and within 4 dB
/// of its own level, and the reply under it does not.
#[test]
fn double_talk_keeps_the_near_end_voice() {
    let (before, after, level) = double_talk("echo_6_db_over_voice", 0.5, 2000.0);
    assert!(after >= 0.75, "{after}");
    assert!(after > before + 0.15, "{after} {before}");
    assert!((-4.0..=1.0).contains(&level), "{level}");
}

/// (c) As measured on the Echo (baselines): the reply's echo and a person
/// speaking arrive at the microphone about equally loud. The voice comes
/// through alike and close to its own level.
#[test]
fn double_talk_with_an_echo_as_loud_as_the_voice_keeps_the_voice() {
    let (before, after, level) = double_talk("echo_as_loud_as_voice", 0.25, 2000.0);
    assert!(after >= 0.8, "{after}");
    assert!(after > before + 0.1, "{after} {before}");
    assert!((-3.0..=1.0).contains(&level), "{level}");
}

/// (c) A quiet voice far from the Echo: the echo is 18 dB louder at the
/// microphone than the voice. AEC3 then gives up some of the voice with
/// the echo. What is left is still far closer to the voice
/// than the microphone signal was.
#[test]
fn double_talk_under_a_louder_echo_still_carries_the_voice() {
    let (before, after, level) = double_talk("echo_18_db_over_voice", 1.0, 1000.0);
    assert!(before < 0.25, "{before}");
    assert!(after >= 0.45, "{after}");
    assert!((-8.0..=1.0).contains(&level), "{level}");
}

/// With nothing played, a voice in the room passes: the canceller only
/// takes out what the far end explains.
#[test]
fn a_voice_with_a_silent_far_end_passes() {
    let near = voice(15, 3 * SECOND, 2000.0);
    let mut canceller = EchoCanceller::new(200);
    let output = cancel(&mut canceller, &vec![0; near.len()], &near);
    let alike = likeness(&near, &output, 400);
    let level = level_db(&output[SECOND..]) - level_db(&near[SECOND..]);
    writeln!(
        std::io::stdout(),
        "silent_far_end near_likeness={alike:.3} near_level_change_db={level:.1}"
    )
    .unwrap();
    // The high-pass filter turns the phase of the lowest harmonics.
    assert!(alike >= 0.55, "{alike}");
    assert!((-1.5..=1.0).contains(&level), "{level}");
    // No far end was audible: nothing was measured as echo.
    let report = canceller.report();
    assert_eq!((report.frames, report.converged, report.erle_db10), (0, false, 0));
}
