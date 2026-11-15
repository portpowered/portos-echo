use std::io::{self, Write};
use std::path::Path;
use std::process::Command;

const TARGETS: [&str; 12] = [
    "fmt-check",
    "lint-rust",
    "lint-shape",
    "lint-deps",
    "lint-rules",
    "lint-scripts-web",
    "lint-docs",
    "test-unit",
    "test-contract",
    "test-sim",
    "build-host",
    "ci",
];

pub fn check(output: &mut impl Write) -> io::Result<bool> {
    evaluate(
        output,
        |target| {
            let result = Command::new("make").args(["-n", target]).output()?;
            if result.status.success() {
                Ok(())
            } else {
                Err(io::Error::other(
                    String::from_utf8_lossy(&result.stderr).trim().to_owned(),
                ))
            }
        },
        Path::new(".github/workflows/ci.yml").is_file(),
    )
}

fn evaluate(output: &mut impl Write, mut run: impl FnMut(&str) -> io::Result<()>, workflow: bool) -> io::Result<bool> {
    let mut passed = workflow;
    for target in TARGETS {
        match run(target) {
            Ok(()) => writeln!(output, "{target}: ok")?,
            Err(error) => {
                passed = false;
                writeln!(output, "{target}: fail: {error}")?;
            }
        }
    }
    let status = if workflow { "ok" } else { "missing" };
    writeln!(output, ".github/workflows/ci.yml: {status}")?;
    Ok(passed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_make_reports_every_failure() {
        let mut output = Vec::new();
        let mut calls = 0;
        assert!(
            !evaluate(
                &mut output,
                |_| {
                    calls += 1;
                    Err(io::Error::new(io::ErrorKind::NotFound, "make unavailable"))
                },
                true
            )
            .unwrap()
        );
        assert_eq!(calls, 12);
        let text = String::from_utf8(output).unwrap();
        assert_eq!(text.matches(": fail: make unavailable").count(), 12);
        assert!(text.ends_with(".github/workflows/ci.yml: ok\n"));
    }

    #[test]
    fn writer_failure_propagates() {
        assert!(evaluate(&mut io::sink(), |_| Ok(()), true).unwrap());
        assert!(evaluate(&mut io::sink(), |_| Ok(()), false).is_ok_and(|ok| !ok));
        let mut buffer = [0_u8; 0].as_mut_slice();
        assert!(evaluate(&mut buffer, |_| Ok(()), true).is_err());
    }
}
