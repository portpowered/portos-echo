// docs/program/rules.md exempts the xtask tool's command-line output.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;

mod ci_parity;
mod coverage;
mod lint_deps;
mod lint_rules;
mod lint_shape;

#[derive(Debug, PartialEq)]
enum Subcommand {
    Deps,
    Rules,
    Shape,
    Parity,
    Coverage,
}

fn dispatch(subcommand: Option<&str>) -> Result<Subcommand, &'static str> {
    match subcommand {
        Some("lint-deps") => Ok(Subcommand::Deps),
        Some("lint-rules") => Ok(Subcommand::Rules),
        Some("lint-shape") => Ok(Subcommand::Shape),
        Some("ci-parity") => Ok(Subcommand::Parity),
        Some("coverage-check") => Ok(Subcommand::Coverage),
        _ => Err("Usage: cargo xtask <subcommand>"),
    }
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let subcommand = args.first().map(String::as_str);
    let expected = if subcommand == Some("coverage-check") { 3 } else { 1 };
    if args.len() != expected {
        eprintln!("Usage: cargo xtask <subcommand>; coverage-check <lcov-path> <base-ref>");
        return ExitCode::from(2);
    }
    let result = match dispatch(subcommand) {
        Ok(Subcommand::Shape) => {
            println!("{}", lint_shape::header());
            let (status, lines) = lint_shape::outcome(lint_shape::run());
            for line in lines {
                println!("{line}");
            }
            return ExitCode::from(status);
        }
        Ok(Subcommand::Deps) => lint_deps::run().map_err(|error| {
            eprintln!("{error}");
            "lint-deps failed"
        }),
        Ok(Subcommand::Rules) => lint_rules::run().map_err(|error| {
            eprintln!("{error}");
            "lint-rules failed"
        }),
        Ok(Subcommand::Parity) => ci_parity::run().map_err(|error| {
            eprintln!("{error}");
            "ci-parity failed"
        }),
        Ok(Subcommand::Coverage) => coverage::run(&args[1], &args[2]).map_err(|error| {
            eprintln!("{error}");
            "coverage-check failed"
        }),
        Err(message) => Err(message),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(if message == "Usage: cargo xtask <subcommand>" {
                2
            } else {
                1
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Subcommand, dispatch};

    #[test]
    fn missing_subcommand_returns_usage() {
        assert_eq!(dispatch(None), Err("Usage: cargo xtask <subcommand>"));
    }

    #[test]
    fn unknown_subcommand_returns_usage() {
        assert_eq!(dispatch(Some("unknown")), Err("Usage: cargo xtask <subcommand>"));
    }

    #[test]
    fn shape_subcommand_is_recognized() {
        assert_eq!(dispatch(Some("lint-shape")), Ok(Subcommand::Shape));
    }

    #[test]
    fn deps_subcommand_is_recognized() {
        assert_eq!(dispatch(Some("lint-deps")), Ok(Subcommand::Deps));
    }

    #[test]
    fn rules_subcommand_selects_rules_instead_of_shape() {
        assert_eq!(dispatch(Some("lint-rules")), Ok(Subcommand::Rules));
    }
}
