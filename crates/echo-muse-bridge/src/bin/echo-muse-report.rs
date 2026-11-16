use std::{io::Write, process::ExitCode};

fn main() -> ExitCode {
    #[cfg(unix)]
    let result = echo_muse_bridge::service::run_handler();
    #[cfg(not(unix))]
    let result: Result<(), &str> = Err("Muse service requires Unix");
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => {
            let _written = writeln!(std::io::stderr(), "{code}");
            ExitCode::FAILURE
        }
    }
}
