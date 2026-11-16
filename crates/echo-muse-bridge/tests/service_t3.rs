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
