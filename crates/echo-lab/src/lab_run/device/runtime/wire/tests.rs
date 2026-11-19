//! T3 component witnesses: synthetic ADB and numeric-loopback socket peers.
use super::*;
use echo_contracts::wyoming::{Event, Header, encode};
use serde_json::Value;
use std::{fs, net::Ipv4Addr};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

fn options(root: &std::path::Path) -> Options {
    Options {
        live: crate::lab_run::run_options::Live::Fake,
        wake_clip: None,
        sim: false,
        bridge: None,
        on_device: None,
        bridge_config: None,
        scenarios: vec![],
        port: PathBuf::new(),
        adb: root.join("adb"),
        stt: PathBuf::new(),
        python: String::new(),
        ring: "synthetic".into(),
        out: root.join("out"),
        keep_audio: false,
        speech: None,
        cancel: None,
        operator_confirm: None,
        allow_quiet_hours: false,
    }
}

pub(super) fn fake_adb(root: &std::path::Path, port: u16) -> Options {
    use std::os::unix::fs::PermissionsExt;
    let options = options(root);
    let ledger = root.join("ledger");
    fs::write(
        &options.adb,
        format!(
            "#!/usr/bin/env python3\nimport json, sys\nwith open({ledger:?}, 'a') as f: f.write(json.dumps(sys.argv[1:]) + '\\n')\nif sys.argv[1:] == ['forward', 'tcp:0', 'tcp:16700']: print({port})\n" // lint-rules: allow-literal synthetic adb argument comparisons, no connection
        ),
    )
    .unwrap();
    fs::set_permissions(&options.adb, fs::Permissions::from_mode(0o700)).unwrap();
    options
}

fn frame(kind: &str, payload: &[u8]) -> Vec<u8> {
    encode(&Event {
        header: Header {
            r#type: kind.into(),
            data: Some(json!({"rate":16000,"width":2,"channels":1})),
            data_length: 0,
            payload_length: payload.len(),
        },
        data: None,
        payload: payload.to_vec(),
    })
}

#[tokio::test]
async fn t3_device_wire_transparent_bidirectional_postwrite_receipts_and_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let options = fake_adb(root.path(), upstream.local_addr().unwrap().port());
    let reserved = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = reserved.local_addr().unwrap();
    drop(reserved);
    let mut observer = Observer::start(&options, address).unwrap();
    observer.launch().unwrap();
    let mut bridge = TcpStream::connect(address).await.unwrap();
    let (mut host, _) = upstream.accept().await.unwrap();
    let capture = [frame("run-pipeline", &[]), frame("audio-chunk", &[1, 2, 3, 4])].concat();
    host.write_all(&capture).await.unwrap();
    let mut received = vec![0; capture.len()];
    bridge.read_exact(&mut received).await.unwrap();
    assert_eq!(received, capture);
    let playback = [frame("audio-start", &[]), frame("hassmic-flush", &[])].concat();
    bridge.write_all(&playback).await.unwrap();
    received.resize(playback.len(), 0);
    host.read_exact(&mut received).await.unwrap();
    assert_eq!(received, playback);
    let played = frame("played", &[]);
    host.write_all(&played).await.unwrap();
    received.resize(played.len(), 0);
    bridge.read_exact(&mut received).await.unwrap();
    assert_eq!(received, played);
    observer.relay.wait_for_receipts(5).await.unwrap();
    let mut artifacts = Artifacts::create(options.out.clone()).unwrap();
    observer.close(&mut artifacts).await.unwrap();
    assert!(TcpStream::connect(address).await.is_err());
    check_receipts(root.path(), upstream.local_addr().unwrap().port());
}

fn check_receipts(root: &std::path::Path, port: u16) {
    let rows: Vec<Value> = fs::read_to_string(root.join("out/device-steps.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[5]["event"], "device.wire_session");
    assert_eq!(rows[5]["streams"][0]["ending"], "hassmic-flush");
    assert_eq!(
        (
            rows[1]["direction"].as_str(),
            rows[1]["kind"].as_str(),
            rows[1]["pcm_bytes"].as_u64()
        ),
        (Some("capture"), Some("audio-chunk"), Some(4))
    );
    assert_eq!(rows[3]["kind"], "hassmic-flush");
    assert_eq!(rows[4]["kind"], "played");
    assert_eq!(rows[4]["wire_run"], 1);
    assert_eq!(rows[4]["playback_stream"], 1);
    check_receipt_order(&rows[..5]);
    assert_eq!(rows[0].as_object().unwrap().len(), 8);
    assert_eq!(rows[0]["sequence"], 1);
    assert_eq!(rows[1]["sequence"], 2);
    assert_eq!(rows[0]["wire_run"], 1);
    assert_eq!(rows[0]["playback_stream"], Value::Null);
    let ledger: Vec<Value> = fs::read_to_string(root.join("ledger"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        ledger,
        vec![
            json!(["forward", "tcp:0", "tcp:16700"]), // lint-rules: allow-literal synthetic adb argument assertion, no connection
            json!(["forward", "--remove", format!("tcp:{port}")]) // lint-rules: allow-literal synthetic adb removal argv assertion
        ]
    );
}

fn check_receipt_order(rows: &[Value]) {
    for pair in rows.windows(2) {
        assert!(pair[1]["elapsed_us"].as_u64().unwrap() >= pair[0]["elapsed_us"].as_u64().unwrap());
        assert_eq!(
            pair[1]["sequence"].as_u64().unwrap(),
            pair[0]["sequence"].as_u64().unwrap() + 1
        );
    }
}

#[tokio::test]
async fn t3_device_wire_occupied_endpoint_has_no_adb_effect_and_never_steals_listener() {
    let root = tempfile::tempdir().unwrap();
    let options = fake_adb(root.path(), 1);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let error = Observer::start(&options, address).err().unwrap();
    assert_eq!(error, "device: unmet prerequisite: nominated wire endpoint unavailable");
    assert!(!root.path().join("ledger").exists());
    assert!(TcpStream::connect(address).await.is_ok());
}

#[test]
fn device_wire_endpoint_is_literal_loopback_and_m1_requires_no_observer() {
    let root = tempfile::tempdir().unwrap();
    let mut options = options(root.path());
    let template = include_str!("../../../../../../../examples/bridge.example.toml");
    assert_eq!(endpoint(&options, "not parsed for M1").unwrap(), None);
    options.scenarios = vec![format!(
        "{}/../../tests/scenarios/m4-spoof-tool.yaml",
        env!("CARGO_MANIFEST_DIR")
    )];
    assert_eq!(
        endpoint(&options, template).unwrap_err(),
        "device: unmet prerequisite: numeric loopback wire endpoint"
    );
    let local = template.replace("echo.example.invalid", "127.0.0.1");
    assert_eq!(
        endpoint(&options, &local).unwrap(),
        Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 16700)))
    );
}

#[tokio::test]
async fn t3_device_wire_failure_joins_observer_and_removes_only_owned_forward() {
    let root = tempfile::tempdir().unwrap();
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let options = fake_adb(root.path(), upstream.local_addr().unwrap().port());
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut observer = Observer::start(&options, address).unwrap();
    observer.launch().unwrap();
    let mut bridge = TcpStream::connect(address).await.unwrap();
    let (_host, _) = upstream.accept().await.unwrap();
    bridge.write_all(b"not-json\n").await.unwrap();
    assert_eq!(bridge.read(&mut [0]).await.unwrap(), 0);
    let mut artifacts = Artifacts::create(options.out).unwrap();
    assert_eq!(
        observer.close(&mut artifacts).await,
        Err("device: malformed wire header".into())
    );
    assert_eq!(observer.relay.receipts(), vec![]);
    assert!(TcpStream::connect(address).await.is_err());
    let ledger = fs::read_to_string(root.path().join("ledger")).unwrap();
    assert_eq!(ledger.lines().count(), 2);
    assert_eq!(
        serde_json::from_str::<Value>(ledger.lines().nth(1).unwrap()).unwrap(),
        json!([
            "forward",
            "--remove",
            format!("tcp:{}", upstream.local_addr().unwrap().port())
        ])
    );
}

#[tokio::test]
async fn t3_device_wire_cancel_before_launch_releases_forward_without_cancellation_guard() {
    let root = tempfile::tempdir().unwrap();
    let mut options = fake_adb(root.path(), 23456);
    options.cancel = Some(root.path().join("cancel"));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut observer = Observer::start(&options, address).unwrap();
    fs::write(options.cancel.as_ref().unwrap(), "cancel").unwrap();
    let mut artifacts = Artifacts::create(options.out).unwrap();
    observer.close(&mut artifacts).await.unwrap();
    assert!(TcpStream::connect(address).await.is_err());
    let ledger = fs::read_to_string(root.path().join("ledger")).unwrap();
    assert_eq!(ledger.lines().count(), 2);
    assert_eq!(
        serde_json::from_str::<Value>(ledger.lines().nth(1).unwrap()).unwrap(),
        json!(["forward", "--remove", "tcp:23456"]) // lint-rules: allow-literal inert fake adb cleanup assertion, no socket
    );
}

#[tokio::test]
async fn t3_device_wire_reply_barrier_awaits_receipt_without_ending_capture() {
    use std::{future::Future, task::Poll};
    let root = tempfile::tempdir().unwrap();
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let options = fake_adb(root.path(), upstream.local_addr().unwrap().port());
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut observer = Observer::start(&options, address).unwrap();
    observer.launch().unwrap();
    let mut bridge = TcpStream::connect(address).await.unwrap();
    let (mut host, _) = upstream.accept().await.unwrap();
    let wake = frame("run-pipeline", &[]);
    host.write_all(&wake).await.unwrap();
    bridge.read_exact(&mut vec![0; wake.len()]).await.unwrap();
    let reply = [
        frame("audio-start", &[]),
        frame("audio-chunk", &[128, 0]),
        frame("audio-stop", &[]),
    ]
    .concat();
    bridge.write_all(&reply).await.unwrap();
    host.read_exact(&mut vec![0; reply.len()]).await.unwrap();
    observer.relay.wait_for_receipts(4).await.unwrap();
    {
        let mut waiting = Box::pin(observer.wait_reply(1, 1));
        std::future::poll_fn(|cx| {
            assert!(waiting.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let played = frame("played", &[]);
        host.write_all(&played).await.unwrap();
        bridge.read_exact(&mut vec![0; played.len()]).await.unwrap();
        waiting.await.unwrap();
    }
    let tail = frame("audio-chunk", &[128, 0]);
    host.write_all(&tail).await.unwrap();
    bridge.read_exact(&mut vec![0; tail.len()]).await.unwrap();
    observer.relay.wait_for_receipts(6).await.unwrap();
    assert_eq!(observer.relay.session()["capture_chunks_after_reply"], 1);
    assert_eq!(observer.relay.session()["ended_runs"], json!([]));
    let mut artifacts = Artifacts::create(options.out).unwrap();
    observer.close(&mut artifacts).await.unwrap();
    assert_eq!(
        observer.wait_reply(1, 1).await,
        Err("device: unmet prerequisite: Played before wire close".into())
    );
    assert!(TcpStream::connect(address).await.is_err());
}

#[tokio::test]
async fn t3_device_wire_reply_barrier_reports_observer_failure_and_still_releases_forward() {
    let root = tempfile::tempdir().unwrap();
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let options = fake_adb(root.path(), upstream.local_addr().unwrap().port());
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut observer = Observer::start(&options, address).unwrap();
    observer.launch().unwrap();
    let mut bridge = TcpStream::connect(address).await.unwrap();
    let (_host, _) = upstream.accept().await.unwrap();
    bridge.write_all(b"not-json\n").await.unwrap();
    assert_eq!(
        observer.wait_reply(1, 1).await,
        Err("device: malformed wire header".into())
    );
    let mut artifacts = Artifacts::create(options.out).unwrap();
    assert_eq!(
        observer.close(&mut artifacts).await,
        Err("device: malformed wire header".into())
    );
    assert!(TcpStream::connect(address).await.is_err());
    assert_eq!(
        fs::read_to_string(root.path().join("ledger")).unwrap().lines().count(),
        2
    );
}

#[tokio::test]
async fn t3_device_wire_late_old_pcm_fails_waiter_and_releases_owned_forward() {
    for ending in ["audio-stop", "hassmic-flush"] {
        let root = tempfile::tempdir().unwrap();
        let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let options = fake_adb(root.path(), upstream.local_addr().unwrap().port());
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mut observer = Observer::start(&options, address).unwrap();
        observer.launch().unwrap();
        let mut bridge = TcpStream::connect(address).await.unwrap();
        let (mut host, _) = upstream.accept().await.unwrap();
        let wake = frame("run-pipeline", &[]);
        host.write_all(&wake).await.unwrap();
        bridge.read_exact(&mut vec![0; wake.len()]).await.unwrap();
        let playback = [
            frame("audio-start", &[]),
            frame("audio-chunk", &[128, 0]),
            frame(ending, &[]),
            frame("audio-chunk", &[0, 1]),
        ]
        .concat();
        bridge.write_all(&playback).await.unwrap();
        let mut received = vec![0; playback.len()];
        host.read_exact(&mut received).await.unwrap();
        assert_eq!(received, playback);
        let failure = "device: old reply PCM after audio-stop or hassmic-flush";
        assert_eq!(observer.wait_reply(1, 1).await, Err(failure.into()));
        let receipt = observer.relay.receipts().last().unwrap().clone();
        assert_eq!((receipt.wire_run, receipt.playback_stream), (Some(1), Some(1)));
        let mut artifacts = Artifacts::create(options.out).unwrap();
        assert_eq!(observer.close(&mut artifacts).await, Err(failure.into()));
        assert!(TcpStream::connect(address).await.is_err());
        let commands = fs::read_to_string(root.path().join("ledger")).unwrap();
        assert_eq!(commands.lines().count(), 2);
        assert!(commands.lines().last().unwrap().contains("--remove"));
    }
}

async fn idle_session(root: &std::path::Path, delay_ms: u64) -> (Observer, Artifacts) {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let mut options = fake_adb(root, upstream.local_addr().unwrap().port());
    options.scenarios = vec![
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/scenarios/session-idle-timeout.yaml")
            .to_str()
            .unwrap()
            .into(),
    ];
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut observer = Observer::start(&options, address).unwrap();
    observer.launch().unwrap();
    let mut bridge = TcpStream::connect(address).await.unwrap();
    let (mut host, _) = upstream.accept().await.unwrap();
    for (capture, bytes) in [
        (true, frame("run-pipeline", &[])),
        (
            false,
            [
                frame("audio-start", &[]),
                frame("audio-chunk", &[128, 0]),
                frame("audio-stop", &[]),
            ]
            .concat(),
        ),
        (true, frame("played", &[])),
    ] {
        let (input, output) = if capture {
            (&mut host, &mut bridge)
        } else {
            (&mut bridge, &mut host)
        };
        input.write_all(&bytes).await.unwrap();
        output.read_exact(&mut vec![0; bytes.len()]).await.unwrap();
    }
    observer.relay.wait_for_receipts(5).await.unwrap();
    tokio::time::advance(std::time::Duration::from_millis(delay_ms)).await;
    let end = frame("hassmic-end", &[]);
    bridge.write_all(&end).await.unwrap();
    host.read_exact(&mut vec![0; end.len()]).await.unwrap();
    observer.relay.wait_for_receipts(6).await.unwrap();
    // Closing these owned socket peers cannot replace an absent explicit end.
    (observer, Artifacts::create(options.out).unwrap())
}

#[tokio::test(start_paused = true)]
async fn t3_device_wire_final_idle_verdict_uses_receipt_clock_and_cleanup_on_failure() {
    for (delay, pass) in [(2900, true), (3100, true), (2899, false), (3101, false)] {
        let root = tempfile::tempdir().unwrap();
        let (mut observer, mut artifacts) = idle_session(root.path(), delay).await;
        let result = observer.close(&mut artifacts).await;
        assert_eq!(artifacts.checks.get("device_session"), Some(&pass));
        if pass {
            result.unwrap();
            let records = fs::read_to_string(artifacts.root.join("device-steps.jsonl")).unwrap();
            let measured: Value = serde_json::from_str(records.lines().next().unwrap()).unwrap();
            assert_eq!(measured["idle_after_played_us"], delay * 1000);
            assert_eq!(measured["session_listen_seconds"], 3);
        } else {
            let failure = result.unwrap_err();
            assert!(failure.contains(&format!("observed {} us", delay * 1000)), "{failure}");
        }
        let commands = fs::read_to_string(root.path().join("ledger")).unwrap();
        assert_eq!(commands.lines().count(), 2);
        assert!(commands.lines().last().unwrap().contains("--remove"));
    }
}
