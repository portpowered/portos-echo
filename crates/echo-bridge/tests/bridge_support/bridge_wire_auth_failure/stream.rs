//! T3: one configured process survives continued PCM after local authorization refusal.
use super::*;
use echo_contracts::wyoming::{Event, Header, encode};
use std::io::Write;

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_auth_failure_continued_microphone_survives() {
    survival(false).await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_auth_failure_paced_microphone_without_screen_survives() {
    survival(true).await;
}

async fn survival(paced: bool) {
    let started = std::time::Instant::now();
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    bridge_wire_screen::setup(directory.path(), listener.local_addr().unwrap().port());
    let fake = FakeLive::bind(FakeScript {
        events: vec![],
        response: FakeResponse::Http {
            status: 401,
            body: String::new(),
            location: None,
        },
    })
    .await
    .unwrap();
    let mut bridge = Process::spawn(&mut bridge_wire_screen::run_command(
        directory.path(),
        fake.base_url().trim_end_matches('/'),
    ));
    let mut socket = listener.accept().await.unwrap().0;
    bridge_short::register(&mut socket, &mut bridge).await;
    let mut satellite = bridge_short::Satellite::new(socket);
    let path = directory.path().join("trace");
    wait_event(&path, "auth.relink_required").await;
    let total = if paced { 313 } else { 3_000 };
    let written = stream_audio(&mut satellite, paced).await;
    // Same-socket control marker precedes run-2. Observing run-2 proves all
    // preceding audio was routed, rather than merely accepted by the OS.
    if written == total {
        bridge_short::send(&mut satellite, "ping", json!({})).await;
        wake(&mut satellite).await;
    }
    let entries = outcome(&path, &mut bridge).await;
    report(&path, written, paced, started);
    assert_eq!(written, total);
    verify(&path, &mut bridge, fake, &entries).await;
}

async fn verify(path: &Path, bridge: &mut Process, fake: FakeLive, entries: &[Value]) {
    let errors = entries.iter().filter(|e| e["event"] == "bridge.error").count();
    if errors != 0 {
        bridge.finish(1).await;
    }
    assert_eq!(errors, 0);
    assert!(bridge.child.try_wait().unwrap().is_none());
    check_second_wake(entries);
    assert_eq!(fake.observer().calls().unwrap().len(), 1);
    let screen = assert_surfaces(path, true).await;
    bridge.finish(0).await;
    assert!(tokio::net::TcpStream::connect(&screen).await.is_err());
    let health = entries[0]["health_addr"].as_str().unwrap();
    assert!(tokio::net::TcpStream::connect(health).await.is_err());
    fake.shutdown().await;
}

fn report(path: &Path, written: usize, paced: bool, started: std::time::Instant) {
    let samples = if paced {
        written.min(312) * 1024 + usize::from(written == 313) * 512
    } else {
        written * 160
    };
    writeln!(
        std::io::stderr(),
        "samples={samples} chunks={written} elapsed={:?}\n{}",
        started.elapsed(),
        std::fs::read_to_string(path).unwrap()
    )
    .unwrap();
}

fn check_second_wake(entries: &[Value]) {
    for event in ["wyoming.run_started", "run.refused"] {
        let rows: Vec<_> = entries
            .iter()
            .filter(|e| e["event"] == event && e["run_id"] == "run-2")
            .collect();
        assert_eq!(rows.len(), 1);
        if event == "run.refused" {
            assert_eq!(rows[0]["reason"], "relink_required");
        }
    }
}

async fn stream_audio(satellite: &mut bridge_short::Satellite, paced: bool) -> usize {
    // hassmic main.c forwards at most 1024 samples per proto->audio call.
    // The paced workload covers exactly 20 s (320,000 samples), with no
    // screen subscriber until after run-2. Timers pace input, not readiness.
    let start = tokio::time::Instant::now();
    let total = if paced { 313 } else { 3_000 };
    for index in 0..total {
        let samples = if paced {
            if index == 312 { 512 } else { 1024 }
        } else {
            160
        };
        if paced {
            let elapsed = ((index + 1) * 64).min(20_000);
            tokio::time::sleep_until(start + Duration::from_millis(elapsed as u64)).await;
        }
        let chunk = encode(&Event {
            header: Header {
                r#type: "audio-chunk".into(),
                data: Some(json!({"rate":16000,"width":2,"channels":1})),
                data_length: 0,
                payload_length: 0,
            },
            data: None,
            payload: vec![0; samples * 2],
        });
        if satellite.write_all(&chunk).await.is_err() {
            return index;
        }
    }
    total
}

async fn outcome(path: &Path, bridge: &mut Process) -> Vec<Value> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let entries = trace(path);
            if entries
                .iter()
                .any(|e| e["event"] == "bridge.error" || (e["event"] == "run.refused" && e["run_id"] == "run-2"))
                || bridge.child.try_wait().unwrap().is_some()
            {
                return entries;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .unwrap()
}
