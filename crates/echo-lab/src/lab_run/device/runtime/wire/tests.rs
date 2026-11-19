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

fn fake_adb(root: &std::path::Path, port: u16) -> Options {
    use std::os::unix::fs::PermissionsExt;
    let options = options(root);
    let ledger = root.join("ledger");
    fs::write(&options.adb, format!(
        "#!/usr/bin/env python3\nimport json, sys\nwith open({ledger:?}, 'a') as f: f.write(json.dumps(sys.argv[1:]) + '\\n')\nif sys.argv[1:] == ['forward', 'tcp:0', 'tcp:16700']: print({port})\n"
    )).unwrap();
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
    .unwrap()
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
    let capture = frame("audio-chunk", &[1, 2, 3, 4]);
    host.write_all(&capture).await.unwrap();
    let mut received = vec![0; capture.len()];
    bridge.read_exact(&mut received).await.unwrap();
    assert_eq!(received, capture);
    let playback = frame("hassmic-flush", &[]);
    bridge.write_all(&playback).await.unwrap();
    received.resize(playback.len(), 0);
    host.read_exact(&mut received).await.unwrap();
    assert_eq!(received, playback);
    observer.relay.wait_for_receipts(2).await.unwrap();
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
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (
            rows[0]["direction"].as_str(),
            rows[0]["kind"].as_str(),
            rows[0]["pcm_bytes"].as_u64()
        ),
        (Some("capture"), Some("audio-chunk"), Some(4))
    );
    assert_eq!(rows[1]["kind"], "hassmic-flush");
    assert!(rows[1]["elapsed_us"].as_u64().unwrap() >= rows[0]["elapsed_us"].as_u64().unwrap());
    assert_eq!(rows[0].as_object().unwrap().len(), 5);
    let ledger: Vec<Value> = fs::read_to_string(root.join("ledger"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        ledger,
        vec![
            json!(["forward", "tcp:0", "tcp:16700"]),
            json!(["forward", "--remove", format!("tcp:{port}")])
        ]
    );
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
    assert!(observer.relay.receipts().is_empty());
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
        json!(["forward", "--remove", "tcp:23456"])
    );
}
