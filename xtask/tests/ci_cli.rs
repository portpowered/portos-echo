#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::{Command, Output};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Session(PathBuf);

    impl Session {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "echo-ci-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn write(&self, path: &str, content: &str) {
            let destination = self.0.join(path);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::write(destination, content).unwrap();
        }

        fn git(&self, args: &[&str]) {
            let output = Command::new("git").args(args).current_dir(&self.0).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        }

        fn run(&self, args: &[&str]) -> (i32, String) {
            result(
                &Command::new(env!("CARGO_BIN_EXE_xtask"))
                    .args(args)
                    .current_dir(&self.0)
                    .output()
                    .unwrap(),
            )
        }

        fn workspace(&self) {
            self.write(
                "Cargo.toml",
                "[workspace]\nmembers = ['echo-fixture']\nresolver = '3'\n",
            );
            self.write(
                "echo-fixture/Cargo.toml",
                "[package]\nname = 'echo-fixture'\nversion = '0.1.0'\nedition = '2024'\n",
            );
            self.write(
                "Cargo.lock",
                "version = 4\n[[package]]\nname = \"echo-fixture\"\nversion = \"0.1.0\"\n",
            );
            self.write("echo-fixture/src/main.rs", "fn main() {}\n");
            self.git(&["init", "--quiet"]);
            self.git(&["add", "."]);
            self.git(&[
                "-c",
                "user.name=CI",
                "-c",
                "user.email=ci@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ]);
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn result(output: &Output) -> (i32, String) {
        (
            output.status.code().unwrap(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )
    }

    #[test]
    fn command_grammar_and_error_exit_codes() {
        let session = Session::new();
        for args in [
            vec![],
            vec!["unknown"],
            vec!["ci-parity", "extra"],
            vec!["coverage-check"],
            vec!["coverage-check", "file"],
        ] {
            let (code, text) = session.run(&args);
            assert_eq!(code, 2, "{text}");
            assert!(text.contains("Usage: cargo xtask"));
        }
        for (command, diagnostic) in [
            ("ci-parity", "Makefile:"),
            ("lint-shape", "lint-shape: input:"),
            ("lint-deps", "cargo metadata failed"),
            ("lint-rules", "Cargo.lock:"),
        ] {
            let (code, text) = session.run(&[command]);
            assert_eq!(code, 1, "{text}");
            assert!(text.contains(diagnostic), "{text}");
        }
    }

    #[test]
    fn parity_cli_accepts_matching_contract_and_rejects_drift() {
        let session = Session::new();
        session.write("Makefile", include_str!("../../Makefile"));
        let (code, text) = session.run(&["ci-parity"]);
        assert_eq!(code, 1);
        assert!(text.contains("ci.yml:"));
        let workflow = include_str!("../../.github/workflows/ci.yml");
        session.write(".github/workflows/ci.yml", workflow);
        let (code, text) = session.run(&["ci-parity"]);
        assert_eq!(code, 0, "{text}");
        assert!(text.starts_with("ci-parity: ok (15 checks)\nformat: make fmt-check\n"));
        assert!(text.ends_with("build-host: make build-host\n"));
        session.write("Makefile", "ci: fmt-check\n");
        let (code, text) = session.run(&["ci-parity"]);
        assert_eq!(code, 1);
        assert!(text.contains("missing gate lint-rust"));
    }

    #[test]
    fn coverage_cli_reports_real_scopes_and_propagates_evidence_errors() {
        let session = Session::new();
        session.workspace();
        let (code, text) = session.run(&["coverage-check", "missing", "HEAD"]);
        assert_eq!(code, 1);
        assert!(text.contains("missing:"));
        session.write(
            "report.lcov",
            "SF:echo-fixture/src/main.rs\nDA:1,1\nBRDA:1,0,0,1\nBRDA:1,0,1,1\nend_of_record\n",
        );
        let (code, text) = session.run(&["coverage-check", "report.lcov", "HEAD"]);
        assert_eq!(code, 0, "{text}");
        assert!(text.contains("workspace lines: 1/1 (100.00%), floor 80%"));
        assert!(text.contains("workspace branches: 2/2 (100.00%), floor 70%"));
        assert!(text.contains("echo-fixture lines: 1/1"));
        assert!(text.contains("changed executable Rust lines: 0/0"));
        session.write("echo-fixture/src/main.rs", "fn main() { std::process::exit(0); }\n");
        assert!(
            session
                .run(&["coverage-check", "report.lcov", "HEAD"])
                .1
                .contains("changed executable Rust lines: 1/1")
        );
        let (code, text) = session.run(&["coverage-check", "report.lcov", "missing-ref"]);
        assert_eq!(code, 1);
        assert!(text.contains("git rev-parse --verify"));
        session.write("report.lcov", "bad");
        assert_eq!(session.run(&["coverage-check", "report.lcov", "HEAD"]).0, 1);
    }

    #[test]
    fn existing_lint_commands_keep_successful_public_outcomes() {
        let session = Session::new();
        session.workspace();
        session.write(
            "lanes.toml",
            "[[crate]]\nname = 'echo-fixture'\npath = 'echo-fixture'\nkind = 'bin'\n",
        );
        session.write("docs/adr/README.md", "Approved `echo-fixture`\n");
        session.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        for command in ["lint-deps", "lint-rules", "lint-shape"] {
            let (code, text) = session.run(&[command]);
            assert_eq!(code, 0, "{command}: {text}");
        }
    }

    #[test]
    fn retained_package_requires_current_approval_and_recovers() {
        let session = Session::new();
        session.write(
            "Cargo.lock",
            "version = 4\n[[package]]\nname = \"itoa\"\nversion = \"1.0.19\"\n",
        );
        session.write("docs/adr/deps.md", "Approved `itoa`\n");
        session.git(&["init", "--quiet"]);
        session.git(&["add", "."]);
        session.git(&[
            "-c",
            "user.name=CI",
            "-c",
            "user.email=ci@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ]);
        session.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        assert_eq!(session.run(&["lint-rules"]), (0, String::new()));
        session.write("docs/adr/deps.md", "Approval removed\n");
        assert_eq!(
            session.run(&["lint-rules"]),
            (
                1,
                "Cargo.lock: itoa: no ADR in docs/adr/ names it\nlint-rules failed\n".into()
            )
        );
        session.write("docs/adr/deps.md", "Approved `itoa`\n");
        assert_eq!(session.run(&["lint-rules"]), (0, String::new()));
    }

    #[test]
    fn new_package_requires_exact_approval() {
        let session = Session::new();
        session.write(
            "Cargo.lock",
            "[[package]]\nname = \"echo-m0-no-adr\"\nversion = \"0.1.0\"\n",
        );
        for approval in ["", "echo-m0-no-adr-extra", "ECHO-M0-NO-ADR", "echo-m0-no-adr_suffix"] {
            session.write("docs/adr/deps.md", approval);
            assert_eq!(
                session.run(&["lint-rules"]),
                (
                    1,
                    "Cargo.lock: echo-m0-no-adr: no ADR in docs/adr/ names it\nlint-rules failed\n".into()
                )
            );
        }
        session.write("docs/adr/deps.md", "Approved `echo-m0-no-adr`.");
        assert_eq!(session.run(&["lint-rules"]), (0, String::new()));
    }

    #[test]
    fn missing_names_are_sorted_deduplicated_and_only_current() {
        let session = Session::new();
        session.write("Cargo.lock", "[[package]]\nname = \"zeta\"\nversion = \"1\"\n[[package]]\nname = \"alpha\"\nversion = \"1\"\n[[package]]\nname = \"zeta\"\nversion = \"2\"\n");
        session.write("docs/adr/deps.md", "Approved `removed`.");
        assert_eq!(session.run(&["lint-rules"]), (1, "Cargo.lock: alpha: no ADR in docs/adr/ names it\nCargo.lock: zeta: no ADR in docs/adr/ names it\nlint-rules failed\n".into()));
        session.write("Cargo.lock", "version = 4\n");
        assert_eq!(session.run(&["lint-rules"]), (0, String::new()));
    }

    #[test]
    fn missing_current_inputs_fail_closed() {
        let session = Session::new();
        let (code, text) = session.run(&["lint-rules"]);
        assert_eq!(code, 1);
        assert!(text.starts_with("Cargo.lock:"), "{text}");
        session.write("Cargo.lock", "version = 4\n");
        let (code, text) = session.run(&["lint-rules"]);
        assert_eq!(code, 1);
        assert!(text.starts_with("docs/adr/:"), "{text}");
        session.write("docs/adr/deps.md", "Approved `alpha`.");
        session.write("Cargo.lock", "[[package]]\nversion = \"1\"\n");
        assert_eq!(
            session.run(&["lint-rules"]),
            (1, "Cargo.lock: package record missing name\nlint-rules failed\n".into())
        );
    }
}
