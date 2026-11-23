use std::{ffi::OsString, io::Write, path::PathBuf, process::ExitCode};

#[cfg(any(target_os = "android", all(test, target_os = "linux")))]
mod android;
#[cfg(any(target_os = "android", all(test, target_os = "linux")))]
mod files;
#[cfg(any(target_os = "android", all(test, target_os = "linux")))]
mod payload;
#[cfg(any(target_os = "android", all(test, target_os = "linux")))]
mod policy;
#[cfg(all(test, target_os = "linux"))]
mod tests;
#[cfg(any(target_os = "android", all(test, target_os = "linux")))]
mod transaction;
#[cfg(any(target_os = "android", all(test, target_os = "linux")))]
mod wake;

const USAGE: &str = "Usage: echo-bridge install --magiskpolicy ABSOLUTE_PATH [--stage ABSOLUTE_PATH] [--no-reboot]";

struct Install {
    magiskpolicy: PathBuf,
    stage: Option<PathBuf>,
    reboot: bool,
}

pub(super) fn dispatch(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let args: Vec<_> = args.into_iter().collect();
    if let [help] = args.as_slice()
        && (help == "--help" || help == "-h")
    {
        return if writeln!(std::io::stdout(), "{USAGE}").is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let Some(install) = parse(args) else {
        let _ = writeln!(std::io::stderr(), "{USAGE}");
        return ExitCode::from(2);
    };
    match execute(install) {
        Ok(outcome) => {
            let _ = writeln!(std::io::stdout(), "echo-bridge install: {outcome}");
            ExitCode::SUCCESS
        }
        Err(failure) => {
            let _ = writeln!(std::io::stderr(), "echo-bridge install: {failure}");
            ExitCode::FAILURE
        }
    }
}

fn parse(args: Vec<OsString>) -> Option<Install> {
    let mut args = args.into_iter();
    let mut magiskpolicy = None;
    let mut stage = None;
    let mut reboot = true;
    while let Some(argument) = args.next() {
        let slot = match argument.to_str()? {
            "--magiskpolicy" if magiskpolicy.is_none() => &mut magiskpolicy,
            "--stage" if stage.is_none() => &mut stage,
            "--no-reboot" if reboot => {
                reboot = false;
                continue;
            }
            _ => return None,
        };
        let path = args.next()?.into_string().ok()?;
        if !path.starts_with('/')
            || path[1..].split('/').any(|part| {
                part.is_empty()
                    || matches!(part, "." | "..")
                    || !part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            })
        {
            return None;
        }
        *slot = Some(PathBuf::from(path));
    }
    Some(Install {
        magiskpolicy: magiskpolicy?,
        stage,
        reboot,
    })
}

#[cfg(any(target_os = "android", all(test, target_os = "linux")))]
fn execute(install: Install) -> echo_boot::Result<&'static str> {
    android::run(install)
}

#[cfg(not(any(target_os = "android", all(test, target_os = "linux"))))]
fn execute(install: Install) -> echo_boot::Result<&'static str> {
    let _input = (install.magiskpolicy, install.stage, install.reboot);
    Err("requires Android root")
}
