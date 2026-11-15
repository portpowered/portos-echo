use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const VALID: &str = include_str!("../../../tests/scenarios/m0-harness.yaml");
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Session(PathBuf);

impl Session {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!(
            "echo-lab-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn write(&self, path: &str, content: &str) -> Result<(), Box<dyn std::error::Error>> {
        let destination = self.0.join(path);
        fs::create_dir_all(destination.parent().ok_or("missing parent")?)?;
        fs::write(destination, content)?;
        Ok(())
    }

    fn run(&self, args: &[&str]) -> Result<(Output, String), Box<dyn std::error::Error>> {
        let output = Command::new(env!("CARGO_BIN_EXE_echo-lab"))
            .args(args)
            .current_dir(&self.0)
            .output()?;
        let text = String::from_utf8(output.stdout.clone())?;
        Ok((output, text))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn explicit_files_continue_after_schema_parse_and_read_failures() {
    let session = Session::new().unwrap();
    session.write("valid file.yaml", VALID).unwrap();
    session
        .write("missing.yaml", &VALID.replace("milestone: M0\n", ""))
        .unwrap();
    session.write("broken.yaml", "steps: [").unwrap();
    let (output, text) = session
        .run(&[
            "validate",
            "missing.yaml",
            "broken.yaml",
            "absent.yaml",
            "valid file.yaml",
        ])
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(text.contains("missing.yaml: /milestone:"), "{text}");
    assert!(text.contains("broken.yaml: :"));
    assert!(text.contains("absent.yaml: :"));
    assert!(text.ends_with("valid file.yaml: ok\n"));
    let (output, text) = session.run(&["validate", "valid file.yaml"]).unwrap();
    assert!(output.status.success());
    assert_eq!(text, "valid file.yaml: ok\n");
}

#[test]
fn discovery_is_sorted_and_never_vacuously_succeeds() {
    let session = Session::new().unwrap();
    let (output, text) = session.run(&["validate"]).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(text.starts_with("tests/scenarios/: :"));
    fs::create_dir_all(session.0.join("tests/scenarios")).unwrap();
    assert_eq!(session.run(&["validate"]).unwrap().0.status.code(), Some(1));
    session.write("tests/scenarios/z.yaml", VALID).unwrap();
    session.write("tests/scenarios/a.yaml", VALID).unwrap();
    session.write("tests/scenarios/ignored.txt", "invalid").unwrap();
    let (output, text) = session.run(&["validate"]).unwrap();
    assert!(output.status.success());
    assert_eq!(text, "tests/scenarios/a.yaml: ok\ntests/scenarios/z.yaml: ok\n");
}

const TARGETS: &str = "fmt-check lint-rust lint-shape lint-deps lint-rules lint-scripts-web lint-docs test-unit test-contract test-sim build-host ci";

#[test]
fn m0_uses_real_make_dry_run_and_reports_every_check() {
    let session = Session::new().unwrap();
    session
        .write(
            "Makefile",
            &format!(".PHONY: {TARGETS}\n{TARGETS}:\n\ttouch executed\n"),
        )
        .unwrap();
    session.write(".github/workflows/ci.yml", "fixture").unwrap();
    let (output, text) = session.run(&["check", "--milestone", "M0"]).unwrap();
    assert!(output.status.success(), "{text}");
    assert_eq!(text.lines().count(), 13);
    for target in TARGETS.split_whitespace() {
        assert!(text.contains(&format!("{target}: ok\n")));
    }
    assert!(text.ends_with(".github/workflows/ci.yml: ok\n"));
    assert!(!Path::new(&session.0.join("executed")).exists());
    fs::remove_file(session.0.join(".github/workflows/ci.yml")).unwrap();
    let (output, text) = session.run(&["check", "--milestone", "M0"]).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(text.matches(": ok\n").count(), 12);
    assert!(text.ends_with(".github/workflows/ci.yml: missing\n"));
    session.write(".github/workflows/ci.yml", "fixture").unwrap();
    session
        .write(
            "Makefile",
            &format!("{}:\n\ttouch executed\n", TARGETS.replace("fmt-check ", "")),
        )
        .unwrap();
    let (output, text) = session.run(&["check", "--milestone", "M0"]).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(text.starts_with("fmt-check: fail:"));
    assert!(text.contains("ci: ok\n"));
    assert_eq!(text.lines().count(), 13);
}

#[test]
fn command_grammar_rejects_malformed_and_unsupported_requests() {
    let session = Session::new().unwrap();
    for args in [
        vec![],
        vec!["check"],
        vec!["check", "--milestone", "M1"],
        vec!["check", "--milestone", "M0", "extra"],
        vec!["unknown"],
    ] {
        let (output, text) = session.run(&args).unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(text.starts_with("invalid arguments or unsupported milestone\n"));
    }
    let (output, text) = session.run(&["--help"]).unwrap();
    assert!(output.status.success());
    assert!(text.contains("echo-lab validate [paths...]\necho-lab check --milestone M0"));
}
