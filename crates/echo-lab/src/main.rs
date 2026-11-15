mod lab_check;
mod lab_validate;

use std::io::{self, Write};
use std::process::ExitCode;

const USAGE: &str = "echo-lab validate [paths...]\necho-lab check --milestone M0\necho-lab --help";

fn dispatch(args: &[String], output: &mut impl Write) -> io::Result<bool> {
    match args {
        [help] if help == "--help" => {
            writeln!(output, "{USAGE}")?;
            Ok(true)
        }
        [command, paths @ ..] if command == "validate" => lab_validate::validate(paths, output),
        [command, flag, milestone] if command == "check" && flag == "--milestone" && milestone == "M0" => {
            lab_check::check(output)
        }
        _ => {
            writeln!(output, "invalid arguments or unsupported milestone\n{USAGE}")?;
            Ok(false)
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match dispatch(&args, &mut io::stdout().lock()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "echo-lab: {error}");
            ExitCode::FAILURE
        }
    }
}
