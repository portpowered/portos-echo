use super::*;
use crate::lab_run::device::runtime::wire::ledger::Ledger;
use echo_contracts::wyoming::Header;

fn receive(ledger: &mut Ledger, direction: &'static str, kind: &str, payload: &[u8]) -> Result<(), String> {
    ledger.observe(
        direction,
        &Event {
            header: Header {
                r#type: kind.into(),
                data: None,
                data_length: 0,
                payload_length: payload.len(),
            },
            data: None,
            payload: payload.into(),
        },
        u128::try_from(ledger.receipts.len()).unwrap() * 1000,
    )
}

fn event(ledger: &mut Ledger, direction: &'static str, kind: &str) {
    receive(ledger, direction, kind, &[]).unwrap();
}

fn reply(ledger: &mut Ledger, pcm: &[u8], ending: &str) {
    event(ledger, "playback", "audio-start");
    receive(ledger, "playback", "audio-chunk", pcm).unwrap();
    event(ledger, "playback", ending);
    event(ledger, "capture", "played");
}

#[test]
fn device_wire_session_reply_barrier_requires_played_and_keeps_capture_open() {
    let mut ledger = Ledger::default();
    assert_eq!(ledger.session.checkpoint(1, 1), Ok(false));
    event(&mut ledger, "capture", "run-pipeline");
    event(&mut ledger, "playback", "audio-start");
    receive(&mut ledger, "playback", "audio-chunk", &[128, 0]).unwrap();
    event(&mut ledger, "playback", "audio-stop");
    assert_eq!(ledger.session.checkpoint(1, 1), Ok(false));
    event(&mut ledger, "capture", "played");
    assert_eq!(ledger.session.checkpoint(1, 1), Ok(true));
    receive(&mut ledger, "capture", "audio-chunk", &[128, 0]).unwrap();
    assert_eq!(ledger.session.observation()["capture_chunks_after_reply"], 1);
    reply(&mut ledger, &[0, 1], "audio-stop");
    assert_eq!(ledger.session.checkpoint(1, 2), Ok(true));
    let observed = ledger.session.observation();
    assert_eq!(observed["wire_runs"], json!([1]));
    assert_eq!(observed["ended_runs"], json!([]));
    assert_eq!(observed["streams"][1]["pcm_bytes"], 2);
    assert_eq!(observed["streams"][1]["audible_samples"], 1);
}

#[test]
fn device_wire_m4_final_reply_accepts_immediate_end_without_relaxing_session_barrier() {
    let mut ledger = Ledger::default();
    event(&mut ledger, "capture", "run-pipeline");
    reply(&mut ledger, &[128, 0], "audio-stop");
    event(&mut ledger, "playback", "hassmic-end");
    assert_eq!(ledger.session.reply_barrier(1, 1, true), Ok(true));
    assert_eq!(
        ledger.session.reply_barrier(1, 1, false),
        Err("device: session advanced beyond reply barrier".into())
    );
    assert!(
        ledger
            .session
            .reply_barrier(1, 2, true)
            .unwrap_err()
            .contains("expected 2, observed 1")
    );
    event(&mut ledger, "capture", "run-pipeline");
    assert_eq!(
        ledger.session.reply_barrier(1, 1, true),
        Err("device: unmet prerequisite: one complete wire session identity".into())
    );
}

#[test]
fn device_wire_session_silence_and_cut_are_not_completed_replies() {
    let mut ledger = Ledger::default();
    event(&mut ledger, "capture", "run-pipeline");
    reply(&mut ledger, &[0, 0], "audio-stop");
    reply(&mut ledger, &[128, 0], "hassmic-flush");
    assert_eq!(ledger.session.checkpoint(1, 1), Ok(false));
    reply(&mut ledger, &[128, 0], "audio-stop");
    assert_eq!(ledger.session.checkpoint(1, 1), Ok(true));
    assert_eq!(ledger.session.observation()["streams"][1]["ending"], "hassmic-flush");
}

#[test]
fn device_wire_session_early_end_extra_reply_and_wrong_run_fail_barrier() {
    let mut ledger = Ledger::default();
    event(&mut ledger, "capture", "run-pipeline");
    reply(&mut ledger, &[128, 0], "audio-stop");
    reply(&mut ledger, &[128, 0], "audio-stop");
    assert_eq!(
        ledger.session.checkpoint(1, 1),
        Err("device: session advanced beyond reply barrier".into())
    );
    event(&mut ledger, "playback", "hassmic-end");
    assert_eq!(
        ledger.session.checkpoint(1, 2),
        Err("device: session advanced beyond reply barrier".into())
    );
    event(&mut ledger, "capture", "run-pipeline");
    assert_eq!(
        ledger.session.checkpoint(2, 2),
        Err("device: unmet prerequisite: current-run Played identity".into())
    );
}

#[test]
fn device_wire_session_unmatched_played_cannot_supply_a_current_reply() {
    let mut ledger = Ledger::default();
    event(&mut ledger, "capture", "played");
    event(&mut ledger, "capture", "run-pipeline");
    reply(&mut ledger, &[128, 0], "audio-stop");
    assert_eq!(ledger.session.observation()["unmatched_played"], 1);
    assert_eq!(
        ledger.session.checkpoint(1, 1),
        Err("device: unmet prerequisite: current-run Played identity".into())
    );
}

pub(in crate::lab_run::device::runtime::wire) fn waveform() -> Vec<u8> {
    (0..3840_u32)
        .flat_map(|index| {
            let position = index % 1920;
            let edge = position.min(1919 - position).min(240);
            let envelope = (1.0 - (std::f64::consts::PI * f64::from(edge) / 240.0).cos()) / 2.0;
            let frequency = if index < 1920 { 750.0 } else { 1000.0 };
            let value = 983.0 * envelope * (std::f64::consts::TAU * frequency * f64::from(position) / 24000.0).sin();
            format!("{value:.0}").parse::<i16>().unwrap().to_le_bytes()
        })
        .collect()
}

#[test]
fn device_wire_session_wake_chime_is_not_a_reply_and_mixed_chime_fails() {
    let mut ledger = Ledger::default();
    event(&mut ledger, "capture", "run-pipeline");
    event(&mut ledger, "playback", "audio-start");
    let pcm = waveform();
    for chunk in pcm.chunks(960) {
        receive(&mut ledger, "playback", "audio-chunk", chunk).unwrap();
    }
    event(&mut ledger, "playback", "hassmic-flush");
    event(&mut ledger, "capture", "played");
    assert_eq!(ledger.session.checkpoint(1, 1), Ok(false));
    assert_eq!(ledger.session.observation()["streams"][0]["wake_chime"], true);
    reply(&mut ledger, &[128, 0], "audio-stop");
    assert_eq!(ledger.session.checkpoint(1, 1), Ok(true));
    let mut corrupted = Ledger::default();
    event(&mut corrupted, "capture", "run-pipeline");
    event(&mut corrupted, "playback", "audio-start");
    receive(&mut corrupted, "playback", "audio-chunk", &pcm[..960]).unwrap();
    assert_eq!(
        receive(&mut corrupted, "playback", "audio-chunk", &[0; 960]),
        Err("device: unmet prerequisite: complete wake chime waveform".into())
    );
}
