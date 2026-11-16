#![cfg(test)]
//! Component T3: Cargo-prebuilt processes, loopback TLS and private Unix IPC.
#[cfg(unix)]
#[path = "service_cases/failures.rs"]
mod failures;
#[cfg(unix)]
#[path = "service_cases/journey.rs"]
mod journey;
#[cfg(unix)]
#[path = "service_cases/lifecycle.rs"]
mod lifecycle;
#[cfg(unix)]
#[path = "service_support/mod.rs"]
mod service_support;

#[cfg(not(unix))]
#[test]
fn f21_unsupported_service_targets_fail_without_side_effects() {
    for program in [
        env!("CARGO_BIN_EXE_echo-muse-daemon"),
        env!("CARGO_BIN_EXE_echo-muse-report"),
    ] {
        let output = std::process::Command::new(program).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, b"");
        assert_eq!(output.stderr, b"Muse service requires Unix\n");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn f20_two_real_handlers_serialize_duplicate_reports() {
    use serde_json::json;
    use service_support::{Fixture, empty, progress, reject};

    let fixture = Fixture::new();
    let daemon = fixture.start().await;
    let id = fixture.submit(&daemon).await;
    assert_eq!(fixture.event(&daemon, &id).await, progress(&id));
    let input = json!({"task_id":id,"status":"completed","summary":"one"}).to_string();
    let (a, b) = tokio::join!(fixture.report(input.as_bytes()), fixture.report(input.as_bytes()));
    let (accepted, rejected) = if a.status.success() { (a, b) } else { (b, a) };
    assert_eq!(accepted.status.code(), Some(0));
    assert_eq!(
        accepted.stdout,
        json!({"accepted":true,"task_id":id}).to_string().as_bytes()
    );
    assert_eq!(accepted.stderr, b"");
    reject(&rejected, "task_id already reported");
    assert_eq!(
        fixture.event(&daemon, &id).await,
        (200, json!({"type":"completed","task_id":id,"summary":"one"}))
    );
    assert_eq!(fixture.event(&daemon, &id).await, empty());
    daemon.stop(true).await;
}
