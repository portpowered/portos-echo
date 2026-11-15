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
            let session = Self(path);
            session.git(&["init", "--quiet"]);
            session
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
            "SF:echo-fixture/src/main.rs\nDA:1,1\nBRDA:1,0,0,1\nBRDA:1,0,1,1\nend_of_record\n", // lint-rules: allow-literal LCOV parser coverage assertion fixture
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
        assert_eq!(
            session.run(&["lint-rules"]),
            (0, "lint-rules: literal exemptions: 0\n".into())
        );
        session.write("docs/adr/deps.md", "Approval removed\n");
        assert_eq!(
            session.run(&["lint-rules"]),
            (
                1,
                "Cargo.lock: itoa: no ADR in docs/adr/ names it\nlint-rules failed\n".into()
            )
        );
        session.write("docs/adr/deps.md", "Approved `itoa`\n");
        assert_eq!(
            session.run(&["lint-rules"]),
            (0, "lint-rules: literal exemptions: 0\n".into())
        );
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
        assert_eq!(
            session.run(&["lint-rules"]),
            (0, "lint-rules: literal exemptions: 0\n".into())
        );
    }

    #[test]
    fn missing_names_are_sorted_deduplicated_and_only_current() {
        let session = Session::new();
        session.write("Cargo.lock", "[[package]]\nname = \"zeta\"\nversion = \"1\"\n[[package]]\nname = \"alpha\"\nversion = \"1\"\n[[package]]\nname = \"zeta\"\nversion = \"2\"\n");
        session.write("docs/adr/deps.md", "Approved `removed`.");
        assert_eq!(session.run(&["lint-rules"]), (1, "Cargo.lock: alpha: no ADR in docs/adr/ names it\nCargo.lock: zeta: no ADR in docs/adr/ names it\nlint-rules failed\n".into()));
        session.write("Cargo.lock", "version = 4\n");
        assert_eq!(
            session.run(&["lint-rules"]),
            (0, "lint-rules: literal exemptions: 0\n".into())
        );
    }

    #[test]
    fn orphan_schema_and_missing_witness_fail_through_public_cli() {
        let session = Session::new();
        session.workspace();
        session.write("docs/adr/deps.md", "Approved `echo-fixture`.\n");
        session.write("schemas/contracts/event.json", "{}\n");
        session.git(&["add", "schemas"]);
        let (code, text) = session.run(&["lint-rules"]);
        assert_eq!(code, 1, "{text}");
        assert!(
            text.contains("crates/echo-contracts/Cargo.toml:1: schema witness input:"),
            "{text}"
        );
        session.write(
            "crates/echo-contracts/Cargo.toml",
            "[package]\nname = 'echo-contracts'\n",
        );
        let (code, text) = session.run(&["lint-rules"]);
        assert_eq!(code, 1, "{text}");
        assert!(
            text.contains("crates/echo-contracts/tests/contracts_schema.rs:1: schema witness input:"),
            "{text}"
        );
        assert!(text.ends_with("lint-rules failed\n"));
    }

    #[test]
    fn secret_source_rejects_before_schema_compilation_and_recovers() {
        let session = Session::new();
        session.workspace();
        session.write("docs/adr/deps.md", "Approved `echo-fixture`.\n");
        let path = "crates/example/src/lib.rs";
        session.write(path, "// seed\n#[derive(Debug)] struct Holder(Secret<String>);\n");
        session.git(&["add", "crates"]);
        assert_eq!(
            session.run(&["lint-rules"]),
            (
                1,
                format!(
                    "{path}:2: secret-format: Secret must not implement Debug or Display or derive Debug\nlint-rules failed\n"
                )
            )
        );
        session.write(path, "struct Holder(Secret<String>);\n");
        assert_eq!(
            session.run(&["lint-rules"]),
            (0, "lint-rules: literal exemptions: 0\n".into())
        );
        fs::remove_file(session.0.join(path)).unwrap();
        let (code, text) = session.run(&["lint-rules"]);
        assert_eq!(code, 1);
        assert!(text.starts_with(&format!("{path}:1: secret-format input:")), "{text}");
        assert!(text.ends_with("lint-rules failed\n"));
    }

    #[test]
    fn network_literals_fail_recover_and_count_through_public_cli() {
        let session = Session::new();
        session.workspace();
        session.write("docs/adr/deps.md", "Approved `echo-fixture`.\n");
        let path = "crates/example/tests/network.rs";
        let remote = "const URL: &str = \"https://api.example.com\";"; // lint-rules: allow-literal parser CLI source fixture
        session.write(path, &format!("// seed\n{remote}\n"));
        session.git(&["add", "crates"]);
        let (code, text) = session.run(&["lint-rules"]);
        assert_eq!(code, 1);
        assert!(text.starts_with("lint-rules: literal exemptions: 0\n"), "{text}");
        assert!(text.contains(&format!("{path}:2: network-tag:")), "{text}");
        assert!(text.ends_with("lint-rules failed\n"));
        let marker = "// lint-rules: allow-literal parser URL assertion";
        session.write(path, &format!("{remote} {marker}\n{remote} {marker}\n"));
        assert_eq!(
            session.run(&["lint-rules"]),
            (0, "lint-rules: literal exemptions: 2\n".into())
        );
        session.write(path, &format!("{remote} {marker}\n{remote}\n"));
        let (code, text) = session.run(&["lint-rules"]);
        assert_eq!(code, 1);
        assert!(text.starts_with("lint-rules: literal exemptions: 1\n"));
        assert!(text.contains(&format!("{path}:2: network-tag:")));
        session.write(path, "const URL: &str = \"http://localhost:80\";\n");
        assert_eq!(
            session.run(&["lint-rules"]),
            (0, "lint-rules: literal exemptions: 0\n".into())
        );
        fs::remove_file(session.0.join(path)).unwrap();
        let (code, text) = session.run(&["lint-rules"]);
        assert_eq!(code, 1);
        assert!(text.contains(&format!("{path}:1:")), "{text}");
    }

    #[test]
    fn cloud_filename_and_gate_are_both_required_through_cli() {
        let session = Session::new();
        session.workspace();
        session.write("docs/adr/deps.md", "Approved `echo-fixture`.\n");
        let remote = "const URL: &str = \"https://api.example.com\";"; // lint-rules: allow-literal parser cloud CLI source fixture
        let gate = "#![cfg(feature = \"cloud-tests\")]";
        session.write("tests/cloud_api.rs", remote);
        session.git(&["add", "tests"]);
        assert_eq!(session.run(&["lint-rules"]).0, 1);
        session.write("tests/cloud_api.rs", &format!("{gate}\n{remote}"));
        assert_eq!(
            session.run(&["lint-rules"]),
            (0, "lint-rules: literal exemptions: 0\n".into())
        );
        session.write("tests/other.rs", &format!("{gate}\n{remote}"));
        session.git(&["add", "tests"]);
        let (code, text) = session.run(&["lint-rules"]);
        assert_eq!(code, 1);
        assert!(text.contains("tests/other.rs:2: network-tag:"), "{text}");
    }

    #[test]
    fn configured_tests_fail_but_product_only_cfg_passes_through_cli() {
        let session = Session::new();
        session.workspace();
        session.write("docs/adr/deps.md", "Approved `echo-fixture`.\n");
        let path = "crates/example/src/lib.rs";
        let body = "async fn example() { let _endpoint = \"https://api.example.com\"; }"; // lint-rules: allow-literal scanner CLI attribute fixture
        session.write(path, &format!("#[tokio::test(flavor = \"current_thread\")]\n{body}\n"));
        session.git(&["add", "crates"]);
        assert_eq!(
            session.run(&["lint-rules"]),
            (
                1,
                format!(
                    "lint-rules: literal exemptions: 0\n{path}:2: network-tag: non-loopback test literal requires cloud gate or reasoned inert-literal annotation\nlint-rules failed\n"
                )
            )
        );
        session.write(path, &format!("#[cfg(not(test))]\n{body}\n"));
        assert_eq!(
            session.run(&["lint-rules"]),
            (0, "lint-rules: literal exemptions: 0\n".into())
        );
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
