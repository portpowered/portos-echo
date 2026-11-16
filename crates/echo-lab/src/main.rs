mod lab_check;
mod lab_probe;
mod lab_run;
mod lab_validate;

use std::io::{self, Write};
use std::process::ExitCode;

const USAGE: &str = "echo-lab validate [paths...]\necho-lab check --milestone M0\necho-lab probe --adb <path-to-adb> --out <dir>\necho-lab run <scenario>... --driver device --bench ring --port <path> --adb <path> --stt <command> [--ring <id>] [--out <fresh-dir>] [--python <path>] [--keep-audio] [--cancel-file <path>] [--speech-endpoint <id> --speech-model <model>]\necho-lab run <scenario> --driver sim --bridge <executable-path> [--out <fresh-dir>] [--python <path>] [--cancel-file <path>]\necho-lab run --all --driver sim [--out <fresh-root>] [--python <path>] [--cancel-file <path>]\necho-lab --help";

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
    if args.first().is_some_and(|arg| arg == "run") {
        return ExitCode::from(lab_run::run(
            &args[1..],
            &mut io::stdout().lock(),
            &mut io::stderr().lock(),
        ));
    }
    if args.first().is_some_and(|arg| arg == "probe") {
        return ExitCode::from(lab_probe::run(
            &args[1..],
            &mut io::stdout().lock(),
            &mut io::stderr().lock(),
        ));
    }
    match dispatch(&args, &mut io::stdout().lock()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "echo-lab: {error}");
            ExitCode::FAILURE
        }
    }
}
