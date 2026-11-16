//! Operator commands are parsed offline, before run-only resources are created.
use crate::Error;
use echo_config::BridgeConfig;
use echo_signin::{OperatorCommand, Providers};
use std::{io::Write, path::PathBuf};

pub const HELP: &str = "echo-bridge --config PATH --trace PATH [--health-listen 127.0.0.1:PORT]\n\
echo-bridge signin --config PATH [--browser]\n\
echo-bridge import-codex --config PATH --path PATH\n\
echo-bridge status --config PATH";

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
    if !["signin", "import-codex", "status"].contains(&verb) {
        return Ok(None);
    }
    if lab_present {
        return Err(Error::Input);
    }
    let mut config = None;
    let mut path = None;
    let mut browser = false;
    let mut flags = args[1..].iter();
    while let Some(flag) = flags.next() {
        match flag.as_str() {
            "--config" => value(&mut config, flags.next())?,
            "--path" if verb == "import-codex" => value(&mut path, flags.next())?,
            "--browser" if verb == "signin" && !browser => browser = true,
            _ => return Err(Error::Input),
        }
    }
    let operation = match verb {
        "signin" => OperatorCommand::Signin { browser },
        "import-codex" => OperatorCommand::ImportCodex {
            path: path.ok_or(Error::Input)?.into(),
        },
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

impl Command {
    /// Validate configuration before entering the async runtime.
    /// # Errors
    /// Returns only fixed categories, without paths or credential contents.
    pub fn prepare(self) -> Result<(Providers, OperatorCommand), Error> {
        let config = BridgeConfig::load(self.config).map_err(|_| Error::Input)?;
        let providers = echo_signin::resolve_mode(config.live, config.chatgpt_auth).map_err(|_| Error::Input)?;
        Ok((providers, self.operation))
    }
}

/// # Errors
/// Reports a fixed authorization or output category; auth internals remain private.
pub async fn execute(providers: Providers, operation: OperatorCommand, writer: &mut impl Write) -> Result<(), Error> {
    providers.execute(operation, writer).await.map_err(|error| match error {
        echo_signin::SigninError::Output => Error::Control,
        _ => Error::Authorization,
    })
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
                OperatorCommand::KillSwitch { .. } => unreachable!(),
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
        ] {
            assert_eq!(parse(&args(text), false).err(), Some(Error::Input));
        }
        assert_eq!(parse(&args("status --config absent"), true).err(), Some(Error::Input));
    }
}
