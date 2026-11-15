// docs/program/rules.md exempts the xtask tool's command-line output.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;

mod lint_rules;
mod lint_shape;

#[derive(Debug, PartialEq)]
enum Subcommand {
    Rules,
    Shape,
}

fn dispatch(subcommand: Option<&str>) -> Result<Subcommand, &'static str> {
    match subcommand {
        Some("lint-rules") => Ok(Subcommand::Rules),
        Some("lint-shape") => Ok(Subcommand::Shape),
        _ => Err("Usage: cargo xtask <subcommand>"),
    }
}

fn main() -> ExitCode {
    let subcommand = std::env::args().nth(1);
    let result = match dispatch(subcommand.as_deref()) {
        Ok(Subcommand::Shape) => {
            println!("{}", lint_shape::header());
            let (status, lines) = lint_shape::outcome(lint_shape::run());
            for line in lines {
                println!("{line}");
            }
            return ExitCode::from(status);
        }
        Ok(Subcommand::Rules) => lint_rules::run().map_err(|error| {
            eprintln!("{error}");
            "lint-rules failed"
        }),
        Err(message) => Err(message),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Subcommand, dispatch};

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

    #[test]
    fn shape_subcommand_is_recognized() {
        assert_eq!(dispatch(Some("lint-shape")), Ok(Subcommand::Shape));
    }

    #[test]
    fn rules_subcommand_selects_rules_instead_of_shape() {
        assert_eq!(dispatch(Some("lint-rules")), Ok(Subcommand::Rules));
    }
}
