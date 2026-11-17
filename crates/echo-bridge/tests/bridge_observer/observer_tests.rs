//! T1 capture values and T3 real pipes; integration children never compile.
use super::*;

#[test]
fn diagnostic_tail_is_bounded_and_redacts_private_records() {
    let capture = diagnostics::Capture::default();
    capture.append(&vec![b'x'; 65536]);
    capture.append(b"\nBearer synthetic-credential\nC:\\fixture\\trace\n/mnt/c/fixture\naccess_token=synthetic\nwyoming on 12345\n");
    let tail = capture.tail();
    assert!(tail.ends_with("<redacted>\n<redacted>\n<redacted>\n<redacted>\nwyoming on 12345"));
    assert!(!tail.contains("synthetic"));
    assert_eq!(diagnostics::safe(&[0xf0, 0x9f]), "\u{fffd}");
    assert_eq!(diagnostics::safe(b""), "<empty>");
    assert_eq!(
        diagnostics::safe(br#"{"event":"bridge.started","provider_auth_mode":"api_key"}"#),
        r#"{"event":"bridge.started","provider_auth_mode":"api_key"}"#
    );
}

#[test]
fn diagnostic_trace_distinguishes_missing_partial_and_unavailable() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    assert_eq!(diagnostics::file_tail(None), "<unavailable>");
    assert_eq!(diagnostics::file_tail(Some(&path)), "<missing>");
    std::fs::write(&path, b"{\"event\":\"bridge.started\"}\n{\"event\":").unwrap();
    assert_eq!(
        diagnostics::file_tail(Some(&path)),
        "{\"event\":\"bridge.started\"}\n{\"event\":"
    );
    std::fs::write(&path, b"secret=synthetic\nwyoming.connected").unwrap();
    assert_eq!(diagnostics::file_tail(Some(&path)), "<redacted>\nwyoming.connected");
}

#[tokio::test]
#[ignore = "T3 uses a native local child and real pipes"]
async fn observer_drains_pipe_pressure_without_stealing_stdout_and_snapshots_live_timeout() {
    let mut command = Command::new("python");
    command.args(["-u", "-c", "import sys; sys.stderr.write('x' * 262144 + '\\nBearer synthetic-credential\\n'); sys.stderr.flush(); print('ready', flush=True); sys.stdin.readline(); print('completed', flush=True); sys.stdin.read()"]);
    let mut process = Process::spawn(&mut command);
    assert_eq!(process.line().await, "ready");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !process.capture.tail().contains("<redacted>") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let failure = process.observe(Duration::ZERO).await.unwrap_err();
    assert!(
        failure.contains("timed out after 0ns waiting for host port\nprocess: alive"),
        "{failure}"
    );
    assert!(failure.ends_with("pinned host port reported: unavailable"));
    assert!(failure.contains("<redacted>"));
    assert!(!failure.contains("synthetic-credential"));
    assert!(failure.len() < 9000);
    assert_eq!(process.command("continue").await, "completed");
    process.finish(0).await;
}

#[tokio::test]
#[ignore = "T3 uses a native local child and real pipes"]
async fn observer_eof_retains_failure_and_stderr_without_waiting_on_live_stdout() {
    let mut command = Command::new("python");
    command.args([
        "-u",
        "-c",
        "import sys; sys.stderr.buffer.write(b'registration failed\\n'); sys.stderr.flush()",
    ]);
    let mut process = Process::spawn(&mut command);
    process.child.wait().await.unwrap();
    assert_eq!(process.stderr().await, "registration failed\n");
    let failure = process.observe(OBSERVATION_TIMEOUT).await.unwrap_err();
    assert!(
        failure.contains("EOF before response waiting for host port\nprocess: exited (Some(0))"),
        "{failure}"
    );
    assert!(failure.contains("stderr tail:\nregistration failed\nbridge trace tail:\n<unavailable>"));
}

#[tokio::test]
#[ignore = "T3 uses the pinned host and its failure-only diagnostic sideband"]
async fn observer_pinned_host_snapshot_preserves_registration_and_cleanup() {
    let mut host = Process::spawn(&mut host());
    let port = host.line().await;
    let capture = diagnostics::Capture::host(port.parse().unwrap()).unwrap();
    let snapshot = capture.snapshot_before(OBSERVATION_TIMEOUT).await;
    assert!(snapshot.starts_with("process: alive\nlog tail:\n"), "{snapshot}");
    assert!(snapshot.contains("wyoming on "), "{snapshot}");
    host.finish(0).await;
}
