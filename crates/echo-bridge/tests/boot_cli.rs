//! T3 command-line witness using the prebuilt bridge.

use std::{
    io,
    process::{Command, Output},
};

fn run(args: &[&str]) -> io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_echo-bridge"))
        .env_clear()
        .args(args)
        .output()
}

fn check_boot_modes() -> io::Result<()> {
    let usage = b"Usage: echo-bridge boot <firewall|run>\n";
    let help = run(&["boot", "--help"])?;
    assert_eq!(help.status.code(), Some(0));
    assert_eq!(help.stdout, usage);
    assert_eq!(help.stderr, b"");

    for args in [&["boot"][..], &["boot", "invalid"][..], &["boot", "run", "extra"][..]] {
        let invalid = run(args)?;
        assert_eq!(invalid.status.code(), Some(2));
        assert_eq!(invalid.stdout, b"");
        assert_eq!(invalid.stderr, usage);
    }
    for mode in ["firewall", "run"] {
        let unsupported = run(&["boot", mode])?;
        assert_eq!(unsupported.status.code(), Some(1));
        assert_eq!(unsupported.stdout, b"");
        assert_eq!(unsupported.stderr, b"echo-bridge boot: requires Android root\n");
    }
    Ok(())
}

#[test]
fn boot_cli_dispatches_without_changing_core_or_device_help() -> io::Result<()> {
    check_boot_modes()?;
    let device = run(&["device", "--help"])?;
    assert_eq!(device.status.code(), Some(2));
    assert_eq!(device.stdout, b"");
    assert!(
        String::from_utf8(device.stderr)
            .unwrap()
            .lines()
            .last()
            .unwrap()
            .starts_with("usage: hassmic [-P")
    );
    let core = run(&["--help"])?;
    assert_eq!(core.status.code(), Some(0));
    assert_eq!(
        core.stdout,
        format!("{}\n", echo_bridge::bridge_operator::HELP).as_bytes()
    );
    assert_eq!(core.stderr, b"");
    Ok(())
}
