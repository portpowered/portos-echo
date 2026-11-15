// docs/program/rules.md exempts the xtask tool's command-line output.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;

fn dispatch(_subcommand: Option<&str>) -> Result<(), &'static str> {
    Err("Usage: cargo xtask <subcommand>")
}

fn main() -> ExitCode {
    let subcommand = std::env::args().nth(1);
    match dispatch(subcommand.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::dispatch;

    #[test]
    fn missing_subcommand_returns_usage() {
        let error = dispatch(None).err();
        assert_eq!(
            error.expect("missing command must fail"),
            "Usage: cargo xtask <subcommand>"
        );
    }

    #[test]
    fn unknown_subcommand_returns_usage() {
        assert_eq!(
            dispatch(Some("unknown")).err().unwrap(),
            "Usage: cargo xtask <subcommand>"
        );
    }
}
