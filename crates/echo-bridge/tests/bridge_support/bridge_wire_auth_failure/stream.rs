//! T3: one configured process survives continued PCM after local authorization refusal.
use super::*;
use echo_contracts::wyoming::{Event, Header, encode};
use std::io::Write;

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_auth_failure_continued_microphone_survives() {
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
    let observer = fake.observer();
    let mut bridge = Process::spawn(&mut bridge_wire_screen::run_command(
        directory.path(),
        fake.base_url().trim_end_matches('/'),
    ));
    let mut socket = listener.accept().await.unwrap().0;
    bridge_short::register(&mut socket, &mut bridge).await;
    let mut satellite = bridge_short::Satellite::new(socket);
    let path = directory.path().join("trace");
    wait_event(&path, "auth.relink_required").await;
    let written = stream_audio(&mut satellite).await;
    // Same-socket control marker precedes run-2. Observing run-2 proves all
    // preceding audio was routed, rather than merely accepted by the OS.
    if written == 3_000 {
        bridge_short::send(&mut satellite, "ping", json!({})).await;
        wake(&mut satellite).await;
    }
    let entries = outcome(&path, &mut bridge).await;
    writeln!(
        std::io::stderr(),
        "samples={} chunks={written} elapsed={:?}\n{}",
        written * 160,
        started.elapsed(),
        std::fs::read_to_string(&path).unwrap()
    )
    .unwrap();
    let errors = entries.iter().filter(|e| e["event"] == "bridge.error").count();
    if errors != 0 {
        bridge.finish(1).await;
    }
    assert_eq!(written, 3_000);
    assert_eq!(errors, 0);
    assert!(bridge.child.try_wait().unwrap().is_none());
    check_second_wake(&entries);
    assert_eq!(observer.calls().unwrap().len(), 1);
    let screen = assert_surfaces(&path, true).await;
    bridge.finish(0).await;
    assert!(tokio::net::TcpStream::connect(&screen).await.is_err());
    let health = entries[0]["health_addr"].as_str().unwrap();
    assert!(tokio::net::TcpStream::connect(health).await.is_err());
    fake.shutdown().await;
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

async fn stream_audio(satellite: &mut bridge_short::Satellite) -> usize {
    let chunk = encode(&Event {
        header: Header {
            r#type: "audio-chunk".into(),
            data: Some(json!({"rate":16000,"width":2,"channels":1})),
            data_length: 0,
            payload_length: 0,
        },
        data: None,
        payload: vec![0; 160 * 2],
    });
    // 480,000 samples, unpaced. The reader concurrently answers idle probes.
    let mut written = 0;
    for _ in 0..3_000 {
        if satellite.write_all(&chunk).await.is_err() {
            break;
        }
        written += 1;
    }
    written
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
