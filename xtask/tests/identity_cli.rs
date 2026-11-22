//! T0: the delivered lint CLI on one isolated Git-selected scratch checkout.
//! The invoking Cargo build owns the artifact; no build or network inside tests.
#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        process::{Command, Output},
    };

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git(root: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .current_dir(root)
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }

    fn gate(root: &Path, expected: i32) -> Output {
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .current_dir(root)
            .arg("lint-rules")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    #[test]
    #[allow(clippy::disallowed_methods, reason = "Host time for test")]
    fn lint_rules_cli_clean_seeded_root_launch_and_recovered_clean() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let scratch = Scratch(std::env::temp_dir().join(format!("echo-identity-{}-{nonce}", std::process::id())));
        let root = &scratch.0;
        std::fs::create_dir_all(root.join("docs/adr")).unwrap();
        git(root, &["init", "--quiet"]);
        std::fs::write(root.join("Cargo.lock"), "version = 4\n").unwrap();
        gate(root, 0);
        for program in ["echo-bridge", "codex-app-server"] {
            let user = "root";
            std::fs::write(root.join("seed.sh"), format!("runas {user} 3990,inet /bin/{program}\n")).unwrap();
            git(root, &["add", "seed.sh"]);
            let output = gate(root, 1);
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("seed.sh:1: root launch of echo-bridge or codex-app-server is forbidden")
            );
            std::fs::remove_file(root.join("seed.sh")).unwrap();
            gate(root, 0); // A selected but deleted source is ignored.
        }
        std::fs::write(root.join("seed.sh"), "runas nobody 3990,inet /bin/echo-bridge\n").unwrap();
        gate(root, 0);
    }
}
