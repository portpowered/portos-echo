use echo_bridge::{Error, bridge_input, bridge_mcp, bridge_operator, bridge_runtime};
use std::{io::Write, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let lab = ["ECHO_LAB_WYOMING_ADDR", "ECHO_LAB_LIVE_URL", "ECHO_LAB_TRACE_PATH"].map(|key| std::env::var(key).ok());
    let lab_idle = std::env::var("ECHO_LAB_SESSION_IDLE_SECONDS").ok();
    match bridge_mcp::parse(&args, lab.iter().any(Option::is_some) || lab_idle.is_some()) {
        Ok(Some(command)) => return mcp_operator(command),
        Err(error) => return input_error(&error),
        Ok(None) => (),
    }
    match bridge_operator::parse(&args, lab.iter().any(Option::is_some) || lab_idle.is_some()) {
        Ok(Some(command)) => return operator(command),
        Err(error) => return input_error(&error),
        Ok(None) => (),
    }
    let settings = match bridge_input::parse(&args, lab).and_then(|settings| {
        settings
            .map(|mut settings| {
                settings.lab_idle(lab_idle.as_deref())?;
                Ok(settings)
            })
            .transpose()
    }) {
        Ok(Some(settings)) => settings,
        Ok(None) => {
            return outcome(writeln!(std::io::stdout(), "{}", bridge_operator::HELP).map_err(|_| Error::Control));
        }
        Err(error) => {
            return input_error(&error);
        }
    };
    if settings.sim_clock {
        return outcome(bridge_mcp::bridge_clock::execute(settings));
    }
    let connector = echo_wyoming::adapter::connector(settings.wyoming, echo_wyoming::ClientOptions::default());
    outcome(runtime().and_then(|runtime| runtime.block_on(bridge_runtime::execute(settings, connector))))
}

fn mcp_operator(command: bridge_mcp::Command) -> ExitCode {
    let command = match command.prepare() {
        Ok(command) => command,
        Err(error) => return input_error(&error),
    };
    outcome(runtime().and_then(|runtime| runtime.block_on(command.execute(&mut std::io::stdout()))))
}

fn runtime() -> Result<tokio::runtime::Runtime, Error> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| Error::Unavailable)
}

fn operator(command: bridge_operator::Command) -> ExitCode {
    let operation = match command.prepare() {
        Ok(prepared) => prepared,
        Err(error) => return input_error(&error),
    };
    outcome(runtime().and_then(|runtime| runtime.block_on(bridge_operator::execute(operation, &mut std::io::stdout()))))
}

fn input_error(error: &Error) -> ExitCode {
    let _ = writeln!(std::io::stderr(), "echo-bridge: {error}");
    ExitCode::from(2)
}

fn outcome(result: Result<(), Error>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "echo-bridge: {error}");
            match error {
                Error::Input
                | Error::CodexHomeRequired
                | Error::CodexHome
                | Error::CodexBinary
                | Error::CodexCaFile
                | Error::CodexImport => ExitCode::from(2),
                _ => ExitCode::FAILURE,
            }
        }
    }
}
