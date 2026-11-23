use echo_bridge::{Error, bridge_input, bridge_mcp, bridge_operator, bridge_runtime, bridge_time::clock_command};
use std::{ffi::OsString, io::Write, process::ExitCode};

#[path = "boot/mod.rs"]
mod boot;

fn main() -> ExitCode {
    let mut args = std::env::args_os();
    if args.next().is_some_and(|program| echo_device::satellite_name(&program)) {
        return device(args);
    }
    let mut args = args.peekable();
    match args.peek().and_then(|arg| arg.to_str()) {
        Some("device") => return device(args.skip(1)),
        Some("boot") => return boot::dispatch(args.skip(1)),
        #[cfg(any(target_os = "android", target_os = "linux"))]
        Some("__boot-launch") if cfg!(target_os = "android") => return boot::launch(args.skip(1)),
        Some("wifi") => return ExitCode::from(echo_bridge::device_wifi::usb_command(args.skip(1))),
        Some("notices") => {
            if args.nth(1).is_some() {
                return input_error(&Error::Input);
            }
            return outcome(echo_device::notices(&mut std::io::stdout()).map_err(|_| Error::Control));
        }
        _ => (),
    }
    match args.map(OsString::into_string).collect::<Result<Vec<_>, _>>() {
        Ok(args) => bridge(&args),
        Err(_) => input_error(&Error::Input),
    }
}

fn device(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    match echo_device::run_with_wifi(args, echo_bridge::device_wifi::HANDLER) {
        Ok(never) => match never {},
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "echo-bridge device: {error}");
            ExitCode::from(2)
        }
    }
}

fn bridge(args: &[String]) -> ExitCode {
    let lab = ["ECHO_LAB_WYOMING_ADDR", "ECHO_LAB_LIVE_URL", "ECHO_LAB_TRACE_PATH"].map(|key| std::env::var(key).ok());
    let lab_idle = std::env::var("ECHO_LAB_SESSION_IDLE_SECONDS").ok();
    let lab_socket = std::env::var_os("ECHO_LAB_WYOMING_SOCKET");
    let lab_requested = lab.iter().any(Option::is_some) || lab_idle.is_some() || lab_socket.is_some();
    match bridge_mcp::parse(args, lab_requested) {
        Ok(Some(command)) => return mcp_operator(command),
        Err(error) => return input_error(&error),
        Ok(None) => (),
    }
    match clock_command::parse(args, lab_requested) {
        Ok(Some(command)) => return outcome(command.execute(&mut std::io::stdout())),
        Err(error) => return input_error(&error),
        Ok(None) => (),
    }
    match bridge_operator::parse(args, lab_requested) {
        Ok(Some(command)) => return operator(command),
        Err(error) => return input_error(&error),
        Ok(None) => (),
    }
    let settings = match bridge_input::parse_with_lab_socket(args, lab, lab_socket).and_then(|settings| {
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
    if let Some(startup) = &settings.startup {
        for warning in &startup.deprecations {
            let _ = writeln!(std::io::stderr(), "echo-bridge: {warning}");
        }
    }
    if settings.sim_clock {
        return outcome(bridge_mcp::bridge_clock::execute(settings));
    }
    if bridge_runtime::configured(&settings) {
        return outcome(echo_bridge::bridge_voice::execute_process(settings));
    }
    outcome(runtime().and_then(|runtime| runtime.block_on(bridge_runtime::execute_wyoming(settings))))
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
                Error::Prompt(_)
                | Error::PromptConfig(_)
                | Error::Input
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
