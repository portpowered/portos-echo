//! T3 native lifecycle boundary: recording adb, private per-case state, no device.
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "echo-lifecycle-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root)?;
        fs::write(root.join("devices"), "SYNTHETIC-DEVICE\tdevice\r\n")?;
        Ok(Self(root))
    }

    fn run(&self, command: &str, extra: &[&str]) -> Result<Output> {
        Ok(Command::new(env!("CARGO_BIN_EXE_echo-setup"))
            .env("ECHO_SETUP_TEST_ROOT", &self.0)
            .args([command, "--json", "--adb", env!("CARGO_BIN_EXE_echo-setup-test-adb")])
            .args(extra)
            .output()?)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn event(output: &Output) -> Result<serde_json::Value> {
    let combined = format!(
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
        assert!(!combined.contains(marker));
    }
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 1);
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[test]
fn t3_status_normalizes_identity_and_line_endings_and_discards_private_logs() -> Result {
    for newline in ["\n", "\r\n"] {
        for user in ["nobody", "9999", "root", "0", "SYNTHETIC-ACCOUNT"] {
            let fixture = Fixture::new()?;
            let safe = matches!(user, "nobody" | "9999");
            let raw = format!(
                "process=running\nprocess_user={user}\nuptime_seconds=42\nhassmic_link=client connected SYNTHETIC-DEVICE\nbridge.log (last 5):\nSYNTHETIC-TOKEN SYNTHETIC-ACCOUNT SYNTHETIC-PRIVATE-PATH\n"
            );
            fs::write(fixture.0.join("status"), raw.replace('\n', newline))?;
            if !safe {
                fs::write(fixture.0.join("status-refused"), b"")?;
            }
            let output = fixture.run("status", &[])?;
            assert_eq!(output.status.code(), Some(i32::from(!safe)));
            let result = event(&output)?;
            let expected = match user {
                "nobody" | "9999" => "nobody",
                "root" | "0" => "root",
                _ => "unavailable",
            };
            assert_eq!(result["process_user"], expected);
            assert_eq!(result["uptime_seconds"], 42);
            assert_eq!(result["ok"], safe);
            assert_eq!(result["hassmic_link"], "connected");
            assert_eq!(result["versions"], serde_json::json!({"current":null,"previous":null}));
            assert_eq!(
                result["last_error"],
                if safe {
                    serde_json::Value::Null
                } else {
                    "unsafe_process_user".into()
                }
            );
            assert_eq!(
                fs::read(fixture.0.join("adapter-input"))?,
                include_bytes!("../../../scripts/echo-install.sh")
            );
        }
    }
    Ok(())
}

#[test]
fn t3_stopped_status_has_no_invented_values_and_malformed_or_failed_observation_refuses() -> Result {
    let fixture = Fixture::new()?;
    let raw = "process=stopped\nprocess_user=unavailable\nuptime_seconds=unavailable\nhassmic_link=unavailable\nbridge.log (last 5):\nunavailable\n";
    fs::write(fixture.0.join("status"), raw)?;
    let output = fixture.run("status", &[])?;
    assert_eq!(output.status.code(), Some(0));
    let result = event(&output)?;
    assert_eq!(result["process"], "stopped");
    assert_eq!(result["process_user"], "unavailable");
    for key in ["uptime_seconds", "hassmic_link", "last_error"] {
        assert!(result[key].is_null());
    }
    fs::write(fixture.0.join("status-refused"), b"")?;
    let output = fixture.run("status", &[])?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(event(&output)?["error"], "status_failed");
    fs::remove_file(fixture.0.join("status-refused"))?;
    fs::write(fixture.0.join("status"), b"SYNTHETIC-TOKEN")?;
    let output = fixture.run("status", &[])?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(event(&output)?["error"], "status_failed");
    Ok(())
}

#[test]
fn t3_uninstall_dispatches_only_authored_shell_and_does_not_report_failed_removal_as_success() -> Result {
    for purge in [false, true] {
        let fixture = Fixture::new()?;
        let extra = if purge { vec!["--purge"] } else { vec![] };
        let output = fixture.run("uninstall", &extra)?;
        assert_eq!(output.status.code(), Some(0));
        assert_eq!(event(&output)?["purged"], purge);
        let operation = fs::read_to_string(fixture.0.join("last-operation"))?;
        assert_eq!(
            operation,
            if purge {
                "sh -s -- --device uninstall --purge"
            } else {
                "sh -s -- --device uninstall"
            }
        );
        fs::write(fixture.0.join("operation-failed"), b"")?;
        let output = fixture.run("uninstall", &extra)?;
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(event(&output)?["error"], "uninstall_failed");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn t3_native_lifecycle_executes_real_authored_shell() -> Result {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("python3")
        .current_dir(root)
        .args([
            "tests/test_echo_install.py",
            "--native-lifecycle",
            env!("CARGO_BIN_EXE_echo-setup"),
        ])
        .output()?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)?,
        "echo-setup native shell: status=PASS identity=PASS uninstall=PASS retention=PASS privacy=PASS\n"
    );
    Ok(())
}
