use echo_bridge::{bridge_input, bridge_runtime};
use std::{io::Write, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let lab = ["ECHO_LAB_WYOMING_ADDR", "ECHO_LAB_LIVE_URL", "ECHO_LAB_TRACE_PATH"].map(|key| std::env::var(key).ok());
    let settings = match bridge_input::parse(&args, lab).and_then(|settings| {
        settings
            .map(|mut settings| {
                settings.lab_idle(std::env::var("ECHO_LAB_SESSION_IDLE_SECONDS").ok().as_deref())?;
                Ok(settings)
            })
            .transpose()
    }) {
        Ok(Some(settings)) => settings,
        Ok(None) => {
            let _ = writeln!(
                std::io::stdout(),
                "echo-bridge --config PATH --trace PATH [--health-listen 127.0.0.1:PORT]"
            );
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "echo-bridge: {error}");
            return ExitCode::from(2);
        }
    };
    let result = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| echo_bridge::Error::Unavailable)
        .and_then(|runtime| runtime.block_on(bridge_runtime::execute(settings)));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "echo-bridge: {error}");
            ExitCode::FAILURE
        }
    }
}
