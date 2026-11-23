//! T3 process witness for the prebuilt device and notices commands.

#[path = "device_cli/wifi.rs"]
mod wifi;

use std::{
    error::Error,
    io,
    process::{Command, Output},
};

const USAGE: &str = "usage: hassmic [-P esphome|wyoming] [-p port] [-n name] [-w local|remote] [-m manifest] [-b input-device] [-z port] [-o port] [-a port] [-i port] [-W port] [-G dir] [-L] [-E] [-V] [-S] [-D]";

fn bridge(args: &[&str]) -> io::Result<Output> {
    let state = tempfile::tempdir()?;
    Command::new(env!("CARGO_BIN_EXE_echo-bridge"))
        .current_dir(state.path())
        .env_clear()
        .env("HASSMIC_STATE", state.path())
        .args(args)
        .output()
}

#[test]
fn device_help_and_invalid_option_use_satellite_parser() -> Result<(), Box<dyn Error>> {
    for flag in ["--help", "--invalid-test-option"] {
        let result = bridge(&["device", flag])?;
        assert_eq!(result.status.code(), Some(2));
        assert_eq!(result.stdout, b"");
        let stderr = String::from_utf8(result.stderr)?;
        assert_eq!(stderr.lines().last(), Some(USAGE));
    }
    Ok(())
}

#[test]
fn notices_work_offline_and_reject_arguments() -> Result<(), Box<dyn Error>> {
    let result = bridge(&["notices"])?;
    assert_eq!(result.status.code(), Some(0));
    assert_eq!(result.stderr, b"");
    let stdout = String::from_utf8(result.stdout)?;
    let mit = include_str!("../../../third_party/hassmic/LICENSE");
    assert!(stdout.starts_with(&format!("hassmic\n{mit}")));

    let result = bridge(&["notices", "extra"])?;
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(result.stdout, b"");
    assert_eq!(
        result.stderr,
        format!("echo-bridge: {}\n", echo_bridge::Error::Input).as_bytes()
    );
    Ok(())
}
