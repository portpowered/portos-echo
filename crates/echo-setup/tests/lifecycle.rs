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
                echo_setup::DEVICE_SCRIPT.as_bytes()
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
fn t3_status_projects_only_paired_canonical_versions_before_the_log_boundary() -> Result {
    let fixture = Fixture::new()?;
    let raw = "process=stopped\nprocess_user=unavailable\nuptime_seconds=unavailable\nhassmic_link=unavailable\n";
    for (current, previous) in [("0.2.0", "0.1.0"), ("0.2.0-rc.1", "null"), ("null", "null")] {
        let frames = format!("version_current={current}\nversion_previous={previous}\n");
        fs::write(
            fixture.0.join("status"),
            format!("{frames}{raw}bridge.log (last 5):\nversion_current=SYNTHETIC-TOKEN\n"),
        )?;
        let output = fixture.run("status", &[])?;
        assert_eq!(output.status.code(), Some(0));
        let versions = event(&output)?["versions"].clone();
        for (key, expected) in [("current", current), ("previous", previous)] {
            assert_eq!(
                versions[key],
                if expected == "null" {
                    serde_json::Value::Null
                } else {
                    expected.into()
                }
            );
        }
    }
    for frames in [
        "version_current=0.2.0\n",
        "version_previous=0.1.0\n",
        "version_current=v0.2.0\nversion_previous=null\n",
        "version_current=0.02.0\nversion_previous=null\n",
        "version_current=0.2.0-01\nversion_previous=null\n",
        "version_current=0.2.0\nversion_previous=SYNTHETIC-TOKEN\n",
        "version_current=0.2.0\nversion_previous=null\nversion_current=0.1.0\n",
    ] {
        fs::write(fixture.0.join("status"), format!("{frames}{raw}"))?;
        let output = fixture.run("status", &[])?;
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(event(&output)?["error"], "status_failed");
    }
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

#[test]
#[allow(clippy::disallowed_methods, reason = "Host time for test")]
fn t3_doctor_reports_each_failure_with_fixed_fixes_and_no_external_data() -> Result {
    let names = [
        "adb",
        "device",
        "hassmic",
        "egress",
        "clock",
        "disk_free",
        "process_user",
    ];
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let healthy = format!("hassmic=ok\negress=ok\nclock={now}\ndisk_free=262144\nprocess_user=9999\n");
    for newline in ["\n", "\r\n"] {
        for failed in 2..=7 {
            let fixture = Fixture::new()?;
            let mut raw = healthy.clone();
            if failed < 7 {
                let line = raw.lines().nth(failed - 2).ok_or("missing probe")?;
                raw = raw.replace(line, &format!("{}=SYNTHETIC-TOKEN", names[failed]));
            }
            fs::write(fixture.0.join("doctor"), raw.replace('\n', newline))?;
            let output = fixture.run("doctor", &[])?;
            assert_eq!(output.status.code(), Some(i32::from(failed != 7)));
            let result = event(&output)?;
            let checks = result["checks"].as_array().ok_or("missing checks")?;
            assert_eq!(checks.len(), 7);
            for (index, check) in checks.iter().enumerate() {
                assert_eq!(check["name"], names[index]);
                assert_eq!(check["ok"], index != failed);
                assert_eq!(check["fix"].is_null(), index != failed);
            }
            assert_eq!(
                fs::read(fixture.0.join("adapter-input"))?,
                echo_setup::DEVICE_SCRIPT.as_bytes()
            );
        }
    }
    Ok(())
}

#[test]
fn t3_doctor_prerequisite_and_malformed_frames_never_report_success() -> Result {
    for failure in ["adb", "device", "transport", "malformed", "duplicate"] {
        let fixture = Fixture::new()?;
        let missing = fixture.0.join("missing-adb");
        if failure == "device" {
            fs::write(fixture.0.join("devices"), "")?;
        } else if failure == "transport" {
            fs::write(fixture.0.join("operation-failed"), "")?;
        } else {
            let raw = if failure == "duplicate" {
                "egress=ok\negress=ok\n"
            } else {
                "SYNTHETIC-TOKEN"
            };
            fs::write(fixture.0.join("doctor"), raw)?;
        }
        let output = if failure == "adb" {
            Command::new(env!("CARGO_BIN_EXE_echo-setup"))
                .args(["doctor", "--json", "--adb"])
                .arg(missing)
                .output()?
        } else {
            fixture.run("doctor", &[])?
        };
        assert_eq!(output.status.code(), Some(1));
        let result = event(&output)?;
        assert_eq!(result["ok"], false);
        for (index, check) in result["checks"].as_array().ok_or("missing checks")?.iter().enumerate() {
            let ok = index == 0 && failure != "adb" || index == 1 && !["adb", "device"].contains(&failure);
            assert_eq!(check["ok"], ok);
            assert_eq!(check["fix"].is_null(), ok);
        }
    }
    Ok(())
}

#[test]
fn t3_doctor_text_prints_safe_fix_for_each_failed_check() -> Result {
    let fixture = Fixture::new()?;
    fs::write(fixture.0.join("doctor"), "SYNTHETIC-TOKEN SYNTHETIC-PRIVATE-PATH")?;
    let output = Command::new(env!("CARGO_BIN_EXE_echo-setup"))
        .env("ECHO_SETUP_TEST_ROOT", &fixture.0)
        .args(["doctor", "--adb", env!("CARGO_BIN_EXE_echo-setup-test-adb")])
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stderr, [] as [u8; 0]);
    let text = String::from_utf8(output.stdout)?;
    assert_eq!(text.lines().count(), 7);
    assert!(!text.contains("SYNTHETIC"));
    assert!(text.contains("hassmic: FAIL Install hassmic separately"));
    assert!(text.contains("disk_free: FAIL Free at least 256 MiB"));
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

#[test]
fn t3_rollback_starts_only_after_shell_owned_restore_succeeds() -> Result {
    for failed in [false, true] {
        let fixture = Fixture::new()?;
        if failed {
            fs::write(fixture.0.join("operation-failed"), b"")?;
        }
        let output = fixture.run("rollback", &[])?;
        assert_eq!(output.status.code(), Some(i32::from(failed)));
        let result = event(&output)?;
        assert_eq!(result["ok"], !failed);
        if failed {
            assert_eq!(result["error"], "rollback_failed");
            assert!(
                result["fix"]
                    .as_str()
                    .is_some_and(|fix| fix.contains("verified previous release"))
            );
        }
        let ledger = fs::read_to_string(fixture.0.join("ledger"))?;
        assert_eq!(ledger.contains("--device start"), !failed);
        assert!(ledger.contains("--device rollback"));
    }
    Ok(())
}

#[test]
fn t3_rollback_restores_previous_payload_and_version() -> Result {
    let fixture = Fixture::new()?;
    let active = fixture.0.join("active");
    let previous = fixture.0.join("previous");
    let files = [
        "bin/echo-bridge",
        "bin/codex-app-server",
        "bin/cacert.pem",
        "apk/echo-screen.apk",
        "scripts/echo-install.sh",
        "scripts/echo-install-release.sh",
        "scripts/echo-install-setup.sh",
        "MANIFEST",
    ];
    for (root, version) in [(&active, "0.2.0"), (&previous, "0.1.0")] {
        fs::create_dir(root)?;
        fs::write(root.join("version"), version)?;
        for name in files {
            let path = root.join(name);
            fs::create_dir_all(path.parent().ok_or("fixture file has no parent")?)?;
            fs::write(path, format!("{version}: {name}"))?;
        }
    }
    let output = fixture.run("rollback", &[])?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(event(&output)?["ok"], true);
    for name in files {
        assert_eq!(fs::read(active.join(name))?, format!("0.1.0: {name}").into_bytes());
    }
    assert!(!previous.exists());
    let output = fixture.run("status", &[])?;
    assert_eq!(output.status.code(), Some(0));
    let state = event(&output)?;
    assert_eq!(
        state["versions"],
        serde_json::json!({"current":"0.1.0","previous":null})
    );
    assert_eq!(state["process_user"], "nobody");
    Ok(())
}

#[test]
fn t3_rollback_refuses_unobserved_or_unsafe_started_process_without_signalling_it() -> Result {
    let healthy = "version_current=0.1.0\nversion_previous=0.2.0\nprocess=running\nprocess_user=nobody\nuptime_seconds=1\nhassmic_link=unavailable\n";
    for (raw, refused, error) in [
        (healthy.replace("nobody", "9999"), false, None),
        (healthy.replace("nobody", "root"), false, Some("unsafe_process_user")),
        (
            healthy.replace("nobody", "SYNTHETIC-ACCOUNT"),
            true,
            Some("unsafe_process_user"),
        ),
        (
            healthy.replace("process=running", "process=stopped"),
            false,
            Some("start_failed"),
        ),
        ("SYNTHETIC-TOKEN".to_owned(), false, Some("start_failed")),
        (healthy.to_owned(), true, Some("start_failed")),
    ] {
        let fixture = Fixture::new()?;
        fs::write(fixture.0.join("status"), raw)?;
        if refused {
            fs::write(fixture.0.join("status-refused"), b"")?;
        }
        let output = fixture.run("rollback", &[])?;
        assert_eq!(output.status.code(), Some(i32::from(error.is_some())));
        let result = event(&output)?;
        assert_eq!(result["ok"], error.is_none());
        if let Some(error) = error {
            assert_eq!(result["error"], error);
        }
        let commands: Vec<Vec<String>> = fs::read_to_string(fixture.0.join("ledger"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?;
        let operations: Vec<_> = commands
            .iter()
            .filter_map(|args| args.last())
            .filter(|arg| arg.contains("--device"))
            .map(String::as_str)
            .collect();
        assert_eq!(
            operations,
            [
                "sh -s -- --device rollback",
                "sh -s -- --device start",
                "sh -s -- --device status"
            ]
        );
    }
    Ok(())
}
