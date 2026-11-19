use super::super::{Observer, tests::fake_adb};
use super::*;
use crate::lab_run::run_report::Artifacts;
use echo_contracts::wyoming::Header;
use echo_contracts::wyoming::encode;
use std::net::Ipv4Addr;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

fn observe(ledger: &mut Ledger, direction: &'static str, kind: &str, at: u128, payload: Vec<u8>) {
    ledger.observe(direction, &event(kind, direction, payload), at).unwrap();
}

fn event(kind: &str, direction: &str, payload: Vec<u8>) -> Event {
    Event {
        header: Header {
            r#type: kind.into(),
            data: Some(json!({"rate":if direction == "capture" {16000} else {24000},
                "width":2,"channels":1})),
            data_length: 0,
            payload_length: payload.len(),
        },
        data: None,
        payload,
    }
}

fn fixture() -> Vec<u8> {
    // Same finite waveform as the M4 fixture owner, with gain changed to 700.
    std::iter::successors(Some(0x1234_5678_u32), |state| {
        let first = state ^ (state << 13);
        let second = first ^ (first >> 17);
        Some(second ^ (second << 5))
    })
    .skip(1)
    .take(1600)
    .flat_map(|state| (if state & 1 == 0 { 700_i16 } else { -700_i16 }).to_le_bytes())
    .collect()
}

fn measured(delay_us: u128) -> Ledger {
    let mut ledger = Ledger::with_latency(true);
    observe(&mut ledger, "capture", "run-pipeline", 0, vec![]);
    for (index, chunk) in fixture().chunks(640).enumerate() {
        observe(
            &mut ledger,
            "capture",
            "audio-chunk",
            u128::try_from(index).unwrap(),
            chunk.into(),
        );
    }
    let first = 4 + delay_us;
    observe(&mut ledger, "playback", "audio-start", first, vec![]);
    observe(&mut ledger, "playback", "audio-chunk", first, vec![128, 0]);
    ledger
}

#[test]
fn device_budget_m4_endpoint_crosses_frames_and_1500ms_is_inclusive() {
    let ledger = measured(1_500_000);
    let value = check(&ledger, 1500.0).unwrap();
    assert_eq!(value["measured"], 1500.0);
    assert_eq!(value["endpoint_us"], 4);
    assert_eq!(value["first_reply_pcm_us"], 1_500_004);
    assert_eq!(value["rule"], "<=");
    assert_eq!(
        check(&measured(1_501_000), 1500.0),
        Err("device: first_audio_after_endpoint_ms_p95 observed 1501 ms exceeds 1500 ms".into())
    );
    assert!(check(&ledger, f64::NAN).is_err());
}

#[test]
fn device_budget_m4_p95_uses_nearest_rank_and_rejects_absent_or_overflow_samples() {
    let samples: Vec<_> = (1..=20_u128).rev().map(|value| value * 1000).collect();
    assert_eq!(p95(&samples), Ok(19.0));
    assert_eq!(p95(&[]), Err("device: unmet prerequisite: M4 latency samples".into()));
    assert_eq!(p95(&[u128::MAX]), Err("device: malformed M4 latency duration".into()));
}

#[test]
fn device_budget_m4_missing_stale_and_unmatched_sources_fail_by_name() {
    let mut ledger = Ledger::with_latency(true);
    assert_eq!(
        check(&ledger, 1500.0),
        Err("device: unmet prerequisite: M4 wire run".into())
    );
    observe(&mut ledger, "capture", "run-pipeline", 0, vec![]);
    observe(&mut ledger, "capture", "audio-chunk", 1, vec![0; 3200]);
    assert_eq!(
        check(&ledger, 1500.0),
        Err("device: unmet prerequisite: matched finite M4 utterance endpoint".into())
    );
    observe(&mut ledger, "capture", "audio-chunk", 2, fixture());
    assert_eq!(
        check(&ledger, 1500.0),
        Err("device: unmet prerequisite: M4 reply PCM".into())
    );
    assert_eq!(
        ledger.observe("capture", &event("run-pipeline", "capture", vec![]), 3),
        Err("device: unmet prerequisite: isolated M4 wire run".into())
    );
    let mut backwards = measured(1000);
    backwards.latency.as_mut().unwrap().endpoint = Some(1005);
    assert_eq!(
        check(&backwards, 1500.0),
        Err("device: unmet prerequisite: M4 reply after endpoint".into())
    );
    backwards.failure = Some("device: malformed wire header".into());
    assert_eq!(check(&backwards, 1500.0), Err("device: malformed wire header".into()));
}

#[test]
fn device_budget_m4_chime_is_excluded_and_quiet_pcm_cannot_establish_reply() {
    let mut ledger = Ledger::with_latency(true);
    observe(&mut ledger, "capture", "run-pipeline", 0, vec![]);
    observe(&mut ledger, "playback", "audio-start", 1, vec![]);
    let pcm = super::super::playback::tests::waveform();
    for chunk in pcm.chunks(960) {
        observe(&mut ledger, "playback", "audio-chunk", 2, chunk.into());
    }
    observe(&mut ledger, "playback", "hassmic-flush", 3, vec![]);
    observe(&mut ledger, "capture", "played", 4, vec![]);
    observe(&mut ledger, "capture", "audio-chunk", 5, fixture());
    assert_eq!(
        check(&ledger, 1500.0),
        Err("device: unmet prerequisite: M4 reply PCM".into())
    );
    observe(&mut ledger, "playback", "audio-start", 6, vec![]);
    observe(&mut ledger, "playback", "audio-chunk", 7, vec![0, 0]);
    assert_eq!(
        check(&ledger, 1500.0),
        Err("device: unmet prerequisite: M4 reply PCM".into())
    );
    observe(&mut ledger, "playback", "audio-chunk", 1005, vec![128, 0]);
    assert_eq!(check(&ledger, 1500.0).unwrap()["first_reply_pcm_us"], 7);
}

#[test]
fn device_budget_m4_format_and_capture_identity_are_prerequisites() {
    let mut ledger = Ledger::with_latency(true);
    assert_eq!(
        ledger.observe("capture", &event("audio-chunk", "capture", fixture()), 0),
        Err("device: unmet prerequisite: current-run M4 capture".into())
    );
    let mut invalid = Ledger::with_latency(true);
    observe(&mut invalid, "capture", "run-pipeline", 0, vec![]);
    let mut input = event("audio-chunk", "capture", fixture());
    input.header.data = Some(json!({"rate":48000,"width":2,"channels":1}));
    assert_eq!(
        invalid.observe("capture", &input, 1),
        Err("device: unmet prerequisite: M4 wire PCM format".into())
    );
}

async fn connected(root: &std::path::Path) -> (Observer, TcpStream, TcpStream) {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let mut options = fake_adb(root, upstream.local_addr().unwrap().port());
    options.scenarios = vec![format!(
        "{}/../../tests/scenarios/m4-spoof-tool.yaml",
        env!("CARGO_MANIFEST_DIR")
    )];
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut observer = Observer::start(&options, address).unwrap();
    observer.launch().unwrap();
    let bridge = TcpStream::connect(address).await.unwrap();
    let (host, _) = upstream.accept().await.unwrap();
    (observer, bridge, host)
}

// T3 component proof: post-write socket receipts, fake ADB, paused wire clock.
async fn socket_measurement(root: &std::path::Path, delay_ms: u64, matched: bool) -> (bool, String) {
    let (mut observer, mut bridge, mut host) = connected(root).await;
    let address = bridge.peer_addr().unwrap();
    let capture = [
        encode(&event("run-pipeline", "capture", vec![])),
        encode(&event(
            "audio-chunk",
            "capture",
            if matched { fixture() } else { vec![0; 3200] },
        )),
    ]
    .concat();
    host.write_all(&capture).await.unwrap();
    bridge.read_exact(&mut vec![0; capture.len()]).await.unwrap();
    observer.relay.wait_for_receipts(2).await.unwrap();
    tokio::time::advance(std::time::Duration::from_millis(delay_ms)).await;
    let playback = [
        encode(&event("audio-start", "playback", vec![])),
        encode(&event("audio-chunk", "playback", vec![128, 0])),
    ]
    .concat();
    bridge.write_all(&playback).await.unwrap();
    host.read_exact(&mut vec![0; playback.len()]).await.unwrap();
    observer.relay.wait_for_receipts(4).await.unwrap();
    let mut artifacts = Artifacts::create(root.join("out")).unwrap();
    let result = observer.close(&mut artifacts).await;
    let passed = artifacts.checks["device_m4_latency"];
    assert!(TcpStream::connect(address).await.is_err());
    let ledger = std::fs::read_to_string(root.join("ledger")).unwrap();
    assert_eq!(ledger.lines().count(), 2);
    assert!(ledger.lines().last().unwrap().contains("--remove"));
    let evidence = result
        .err()
        .unwrap_or_else(|| std::fs::read_to_string(artifacts.root.join("device-steps.jsonl")).unwrap());
    (passed, evidence)
}

#[tokio::test(start_paused = true)]
async fn t3_device_wire_m4_latency_uses_postwrite_clock_and_cleans_up_failed_measurements() {
    for (delay, matched, expected) in [
        (1500, true, "\"measured\":1500.0"),
        (1501, true, "observed 1501 ms exceeds 1500 ms"),
        (
            1500,
            false,
            "device: unmet prerequisite: matched finite M4 utterance endpoint",
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let (passed, evidence) = socket_measurement(root.path(), delay, matched).await;
        assert_eq!(passed, delay == 1500 && matched);
        assert!(evidence.contains(expected), "{evidence}");
    }
}
