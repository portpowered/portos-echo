//! T3 Windows bootstrap: real PowerShell, hashing, native child, and private files.
//! Only Invoke-WebRequest is substituted; real TLS/redirect transport is a separate witness.
#![cfg(windows)]

use echo_setup::integrity::digest;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
static NEXT: AtomicU64 = AtomicU64::new(0);
const ASSET: &str = "echo-setup-0.2.0-windows-x86_64.exe";

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "echo-bootstrap-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root)?;
        fs::create_dir(root.join("temp"))?;
        fs::copy(env!("CARGO_BIN_EXE_echo-setup-test-adb"), root.join(ASSET))?;
        fs::write(root.join("child-exit"), "0")?;
        fs::write(root.join("config ü ' ; $.toml"), "synthetic config")?;
        let sums = format!("{}  {ASSET}\r\n", digest(&fs::read(root.join(ASSET))?));
        fs::write(root.join("SHA256SUMS"), sums)?;
        fs::write(root.join("mode"), "good")?;
        let script = include_str!("../../../scripts/install.ps1").replace("@ECHO_VERSION@", "0.2.0");
        fs::write(root.join("install.ps1"), script)?;
        fs::write(root.join("invoke.ps1"), include_str!("support/bootstrap.ps1"))?;
        Ok(Self(root))
    }

    fn invoke(&self, shell: &str, pipe: bool, arguments: &[&str]) -> Result<Output> {
        let output = Command::new(shell)
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(self.0.join("invoke.ps1"))
            .args(arguments)
            .env_remove("PSModulePath")
            .env("ECHO_SETUP_TEST_BOOTSTRAP", &self.0)
            .env("ECHO_SETUP_TEST_PIPE", if pipe { "1" } else { "0" })
            .env(
                "ECHO_SETUP_CONFIG",
                if pipe {
                    self.0.join("config ü ' ; $.toml")
                } else {
                    PathBuf::new()
                },
            )
            .env("TEMP", self.0.join("temp"))
            .env("TMP", self.0.join("temp"))
            .output()?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        for marker in [
            "SYNTHETIC-DEVICE",
            "SYNTHETIC-TOKEN",
            "SYNTHETIC-ACCOUNT",
            "SYNTHETIC-PRIVATE-PATH",
        ] {
            assert!(!text.contains(marker), "private boundary bytes escaped");
        }
        assert!(
            !text.contains(&self.0.to_string_lossy().to_string()),
            "local path escaped"
        );
        assert_eq!(
            fs::read_dir(self.0.join("temp"))?.count(),
            0,
            "download cleanup missing"
        );
        Ok(output)
    }

    fn args(&self) -> Result<Vec<String>> {
        Ok(serde_json::from_slice(&fs::read(self.0.join("installer-args.json"))?)?)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn t3_windows_bootstrap_verified_child_exact_argv_exit_and_pipe() -> Result {
    for shell in ["powershell.exe", "pwsh.exe"] {
        for (pipe, code) in [(false, 0), (true, 0), (false, 3)] {
            let fixture = Fixture::new()?;
            fs::write(fixture.0.join("child-exit"), code.to_string())?;
            let config = fixture.0.join("config ü ' ; $.toml");
            let config = config.to_str().ok_or("invalid path")?;
            let output = fixture.invoke(shell, pipe, &["-Config", config])?;
            assert_eq!(
                output.status.code(),
                Some(code),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(fixture.args()?, ["install", "--version", "0.2.0", "--config", config]);
            assert!(String::from_utf8_lossy(&output.stdout).contains("synthetic installer completed"));
            assert_eq!(
                String::from_utf8_lossy(&output.stderr).contains("synthetic installer needs resume"),
                code != 0
            );
            let requests: Vec<String> = fs::read_to_string(fixture.0.join("requests"))?
                .lines()
                .map(str::to_owned)
                .collect();
            assert_eq!(requests.len(), 2);
            assert!(requests[0].ends_with(ASSET));
            assert!(requests[1].ends_with("SHA256SUMS"));
        }
    }
    Ok(())
}

#[test]
fn t3_windows_bootstrap_checksum_refusals_never_execute() -> Result {
    for shell in ["powershell.exe", "pwsh.exe"] {
        for fault in ["tamper", "missing", "duplicate", "malformed", "extra", "upper"] {
            let fixture = Fixture::new()?;
            let sums = fixture.0.join("SHA256SUMS");
            let original = fs::read_to_string(&sums)?;
            match fault {
                "tamper" => fs::write(fixture.0.join(ASSET), b"tampered")?,
                "missing" => fs::write(sums, "")?,
                "duplicate" => fs::write(sums, original.repeat(2))?,
                "malformed" => fs::write(sums, format!("short  {ASSET}\n"))?,
                "extra" => fs::write(sums, format!("{} extra\n", original.trim()))?,
                "upper" => fs::write(
                    sums,
                    format!(
                        "{}  {ASSET}\n",
                        digest(&fs::read(fixture.0.join(ASSET))?).to_uppercase()
                    ),
                )?,
                _ => unreachable!(),
            }
            let output = fixture.invoke(shell, false, &[])?;
            assert_eq!(output.status.code(), Some(1));
            assert!(String::from_utf8_lossy(&output.stderr).contains("checksum"));
            assert!(!fixture.0.join("installer-args.json").exists());
        }
    }
    Ok(())
}

#[test]
fn t3_windows_bootstrap_redirect_policy_and_download_errors() -> Result {
    for shell in ["powershell.exe", "pwsh.exe"] {
        for mode in [
            "redirect",
            "downgrade",
            "later-downgrade",
            "loop",
            "failure",
            "checksum-downgrade",
            "checksum-later-downgrade",
            "checksum-loop",
            "checksum-failure",
        ] {
            let fixture = Fixture::new()?;
            fs::write(fixture.0.join("mode"), mode)?;
            let output = fixture.invoke(shell, false, &[])?;
            assert_eq!(output.status.code(), Some(i32::from(mode != "redirect")));
            let requests = fs::read_to_string(fixture.0.join("requests"))?;
            assert!(requests.lines().all(|line| line.starts_with("https://")));
            assert!(requests.lines().count() <= 22);
            assert_eq!(fixture.0.join("installer-args.json").exists(), mode == "redirect");
        }
    }
    Ok(())
}

#[test]
fn t3_windows_bootstrap_usage_and_source_version() -> Result {
    for shell in ["powershell.exe", "pwsh.exe"] {
        for arguments in [
            &["-Version", "not-a-version"][..],
            &["-Unknown"],
            &["-Version", "0.2.0", "--version", "0.2.0"],
        ] {
            let fixture = Fixture::new()?;
            let output = fixture.invoke(shell, false, arguments)?;
            assert_eq!(output.status.code(), Some(2));
            assert!(!fixture.0.join("requests").exists());
        }
        let fixture = Fixture::new()?;
        let missing = fixture.0.join("SYNTHETIC-PRIVATE-PATH.toml");
        assert_eq!(
            fixture
                .invoke(shell, false, &["-Config", missing.to_str().ok_or("invalid path")?])?
                .status
                .code(),
            Some(1)
        );
        assert!(!fixture.0.join("requests").exists());
        fs::write(
            fixture.0.join("install.ps1"),
            include_str!("../../../scripts/install.ps1"),
        )?;
        assert_eq!(fixture.invoke(shell, true, &[])?.status.code(), Some(2));
        assert!(!fixture.0.join("requests").exists());
        assert_eq!(fixture.invoke(shell, false, &["--help"])?.status.code(), Some(0));
        assert!(!fixture.0.join("requests").exists());
        assert_eq!(
            fixture.invoke(shell, false, &["--version", "v0.2.0"])?.status.code(),
            Some(0)
        );
    }
    Ok(())
}
