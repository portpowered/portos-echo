//! Operator commands are parsed offline, before run-only resources are created.
use crate::Error;
use echo_config::BridgeConfig;
use echo_signin::OperatorCommand;
pub(crate) mod codex_host;
mod codex_operator;
pub(crate) use codex_operator::screen_account;
mod codex_routes;
pub use codex_routes::LocalRoutes;
use std::{io::Write, path::PathBuf};

pub const HELP: &str = "echo-bridge --config PATH --trace PATH [--health-listen 127.0.0.1:PORT] [--live-loopback ORIGIN --model-loopback ORIGIN] [--sim-clock [--sim-epoch UNIX_SECONDS]]\n\
echo-bridge signin --config PATH [--browser]\n\
echo-bridge import-codex --config PATH --path PATH\n\
echo-bridge status --config PATH\n\
echo-bridge kill --config PATH [--clear]\n\
echo-bridge link --config PATH\n\
echo-bridge --refresh-now --config PATH\n\
echo-bridge unlink --config PATH\n\
echo-bridge clock check|sync [--config PATH] [--threshold-ms N]";

pub struct Command {
    config: PathBuf,
    operation: OperatorCommand,
}

/// Recognizes operator verbs without loading config, reading credentials or dialing.
/// # Errors
/// Rejects duplicate, incomplete, mixed lab and command-inappropriate flags.
pub fn parse(args: &[String], lab_present: bool) -> Result<Option<Command>, Error> {
    let Some(verb) = args.first().map(String::as_str) else {
        return Ok(None);
    };
    if !["signin", "import-codex", "status", "kill"].contains(&verb) {
        return Ok(None);
    }
    if lab_present {
        return Err(Error::Input);
    }
    let mut config = None;
    let mut path = None;
    let mut browser = false;
    let mut clear = false;
    let mut flags = args[1..].iter();
    while let Some(flag) = flags.next() {
        match flag.as_str() {
            "--config" => value(&mut config, flags.next())?,
            "--path" if verb == "import-codex" => value(&mut path, flags.next())?,
            "--browser" if verb == "signin" && !browser => browser = true,
            "--clear" if verb == "kill" && !clear => clear = true,
            _ => return Err(Error::Input),
        }
    }
    let operation = match verb {
        "signin" => OperatorCommand::Signin { browser },
        "import-codex" => OperatorCommand::ImportCodex {
            path: path.ok_or(Error::Input)?.into(),
        },
        "kill" => OperatorCommand::KillSwitch { enabled: !clear },
        _ => OperatorCommand::Status,
    };
    Ok(Some(Command {
        config: config.ok_or(Error::Input)?.into(),
        operation,
    }))
}

fn value(target: &mut Option<String>, input: Option<&String>) -> Result<(), Error> {
    let input = input.filter(|text| !text.is_empty() && !text.starts_with("--"));
    if target.replace(input.ok_or(Error::Input)?.clone()).is_some() {
        return Err(Error::Input);
    }
    Ok(())
}

pub struct Prepared {
    config: BridgeConfig,
    operation: OperatorCommand,
}

impl Command {
    /// Load offline configuration without touching credentials or the app-server home.
    /// # Errors
    /// Fixed categories only; unsupported import never reads its source.
    pub fn prepare(self) -> Result<Prepared, Error> {
        let config = BridgeConfig::load(&self.config).map_err(|_| Error::Input)?;
        if matches!(self.operation, OperatorCommand::ImportCodex { .. }) {
            return Err(Error::CodexImport);
        }
        if !matches!(self.operation, OperatorCommand::KillSwitch { .. }) && config.codex.home.is_none() {
            return Err(Error::CodexHomeRequired);
        }
        Ok(Prepared {
            config,
            operation: self.operation,
        })
    }
}

/// Execute with an owned child and awaited process/screen cleanup.
/// # Errors
/// Fixed configuration, account, output or cleanup diagnostics.
pub async fn execute(command: Prepared, writer: &mut impl Write) -> Result<(), Error> {
    execute_controlled(command, writer, std::future::pending()).await
}

/// Cancellation drops the account future, then awaits cleanup before releasing ownership.
/// # Errors
/// Cancellation is a control error; cleanup failures remain visible.
pub async fn execute_controlled(
    command: Prepared,
    writer: &mut impl Write,
    cancelled: impl std::future::Future<Output = ()>,
) -> Result<(), Error> {
    if let OperatorCommand::KillSwitch { enabled } = command.operation {
        crate::bridge_runtime::bridge_kill::mutate(&command.config.audit.log_dir.join("bridge-kill"), enabled)?;
        let label = if enabled { "on" } else { "off" };
        return writeln!(writer, "kill-switch: {label}")
            .and_then(|()| writer.flush())
            .map_err(|_| Error::Control);
    }
    codex_operator::execute(
        command,
        writer,
        cancelled,
        #[cfg(test)]
        None,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn operator_flags_select_exact_commands_without_reading_missing_paths() {
        for (text, expected) in [
            ("signin --config absent", "device"),
            ("signin --browser --config absent", "browser"),
            ("import-codex --path explicit --config absent", "import"),
            ("status --config absent", "status"),
        ] {
            let command = parse(&args(text), false).unwrap().unwrap();
            assert_eq!(command.config, PathBuf::from("absent"));
            match command.operation {
                OperatorCommand::Signin { browser } => assert_eq!(expected, if browser { "browser" } else { "device" }),
                OperatorCommand::ImportCodex { path } => {
                    assert_eq!(expected, "import");
                    assert_eq!(path, PathBuf::from("explicit"));
                }
                OperatorCommand::Status => assert_eq!(expected, "status"),
                OperatorCommand::Signout | OperatorCommand::KillSwitch { .. } => unreachable!(),
            }
        }
        assert!(parse(&args("--help"), false).unwrap().is_none());
    }

    #[test]
    fn operator_invalid_inputs_and_lab_mixing_fail_offline() {
        for text in [
            "signin",
            "signin --config",
            "signin --config --browser",
            "signin --config x --browser --browser",
            "status --config x --config y",
            "status --config x --browser",
            "status --config x --trace y",
            "import-codex --config x",
            "import-codex --config x --path y --path z",
            "signin --config x --path y",
            "kill --config x --clear --clear",
            "kill --config x --browser",
            "kill --config x --path y",
            "kill --clear",
            "status --config x --clear",
        ] {
            assert_eq!(parse(&args(text), false).err(), Some(Error::Input));
        }
        assert_eq!(parse(&args("status --config absent"), true).err(), Some(Error::Input));
    }

    #[test]
    fn kill_flags_select_exact_state_without_reading_a_credential() {
        for (text, enabled) in [("kill --config absent", true), ("kill --clear --config absent", false)] {
            let command = parse(&args(text), false).unwrap().unwrap();
            assert!(matches!(command.operation, OperatorCommand::KillSwitch { enabled: actual } if actual == enabled));
            assert_eq!(parse(&args(text), true).err(), Some(Error::Input));
        }
    }
}
