use std::{ffi::OsString, io::Write, process::ExitCode};

#[cfg(any(target_os = "android", target_os = "linux"))]
mod android;
#[cfg(any(target_os = "android", target_os = "linux"))]
mod command_io;
#[cfg(any(target_os = "android", target_os = "linux"))]
mod firewall_io;
#[cfg(any(target_os = "android", target_os = "linux"))]
mod launcher;
#[cfg(any(target_os = "android", target_os = "linux"))]
mod layout_io;
#[cfg(any(target_os = "android", target_os = "linux"))]
mod processes;
#[cfg(any(target_os = "android", target_os = "linux"))]
mod receipt;
#[cfg(all(test, target_os = "linux"))]
#[path = "tests/boot_supervision.rs"]
mod tests;

#[cfg(any(target_os = "android", target_os = "linux"))]
pub(super) use launcher::launch;

const USAGE: &str = "Usage: echo-bridge boot <firewall|run>";

#[derive(Clone, Copy)]
enum Mode {
    Firewall,
    Run,
}

pub(super) fn dispatch(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let mut args = args.into_iter();
    let first = args.next();
    if args.next().is_none() {
        match first.as_deref().and_then(|arg| arg.to_str()) {
            Some("--help" | "-h") => {
                return if writeln!(std::io::stdout(), "{USAGE}").is_ok() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                };
            }
            Some("firewall") => return result(run(Mode::Firewall)),
            Some("run") => return result(run(Mode::Run)),
            _ => (),
        }
    }
    let _ = writeln!(std::io::stderr(), "{USAGE}");
    ExitCode::from(2)
}

fn result(result: echo_boot::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(stage) => {
            let _ = writeln!(std::io::stderr(), "echo-bridge boot: {stage}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
fn run(_: Mode) -> echo_boot::Result<()> {
    Err("requires Android root")
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn run(mode: Mode) -> echo_boot::Result<()> {
    launcher::require_root()?;
    let _firewall_lock = match mode {
        Mode::Firewall => Some(layout_io::lock_firewall()?),
        Mode::Run => None,
    };
    execute(mode, &mut android::Android::open(matches!(mode, Mode::Run))?)
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn execute(mode: Mode, host: &mut impl echo_boot::HostOps) -> echo_boot::Result<()> {
    match mode {
        Mode::Firewall => {
            echo_boot::watch_firewall(host);
            Ok(())
        }
        Mode::Run => echo_boot::supervise(host),
    }
}
