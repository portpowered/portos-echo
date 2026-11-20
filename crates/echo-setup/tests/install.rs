//! T3: native CLI/HTTP/recording-adb journey. Device execution is a separate shell witness.
use echo_setup::integrity::{REQUIRED, digest};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
static NEXT: AtomicU64 = AtomicU64::new(0);

#[path = "support/install_setup.rs"]
mod install_setup;

struct Fixture(PathBuf, Option<install_setup::Service>);

impl Fixture {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "echo-setup-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root)?;
        fs::write(root.join("devices"), "SYNTHETIC-DEVICE\tdevice\r\n")?;
        let service = install_setup::Service::new(&root)?;
        Ok(Self(root, Some(service)))
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_echo-setup"));
        command.env("ECHO_SETUP_TEST_ROOT", &self.0);
        command
    }

    fn install(&self) -> Command {
        let mut command = self.command();
        command.args(["install", "--json", "--adb", env!("CARGO_BIN_EXE_echo-setup-test-adb")]);
        command
    }

    fn ledger(&self) -> Result<Vec<Vec<String>>> {
        let path = self.0.join("ledger");
        if !path.exists() {
            return Ok(Vec::new());
        }
        fs::read_to_string(path)?
            .lines()
            .map(|line| Ok(serde_json::from_str(line)?))
            .collect()
    }

    fn bundle(&self) -> Result<PathBuf> {
        let root = self.0.join("bundle ü ' ; $");
        let mut files = BTreeMap::new();
        for name in REQUIRED {
            let path = root.join(name);
            fs::create_dir_all(path.parent().ok_or("missing parent")?)?;
            fs::write(&path, b"synthetic public release bytes")?;
            files.insert(*name, digest(b"synthetic public release bytes"));
        }
        fs::write(
            root.join("MANIFEST"),
            serde_json::to_vec(&serde_json::json!({
                "format":1, "version":"0.2.0", "files":files
            }))?,
        )?;
        Ok(root)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.1.take();
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn private_output(output: &Output) -> Result<serde_json::Value> {
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
        assert!(!text.contains(marker), "private boundary data escaped");
    }
    let events: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    assert_eq!(events.iter().filter(|event| event["type"] == "result").count(), 1);
    Ok(events.last().ok_or("missing terminal result")?.clone())
}

fn server(sums: String, bytes: Vec<u8>, count: usize) -> Result<(String, thread::JoinHandle<io::Result<()>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let mut base = String::from("http://127.0.0.1:");
    write!(base, "{}", listener.local_addr()?.port())?;
    let handle = thread::spawn(move || {
        for _ in 0..count {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte)?;
                request.push(byte[0]);
            }
            let request = String::from_utf8_lossy(&request);
            let body = if request.starts_with("GET /v0.2.0/SHA256SUMS ") {
                sums.as_bytes()
            } else {
                assert!(request.starts_with("GET /v0.2.0/echo-0.2.0-armv7.tar.gz "));
                &bytes
            };
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )?;
            stream.write_all(body)?;
        }
        Ok(())
    });
    Ok((base, handle))
}
#[test]
fn t3_verified_release_pushes_then_dispatches_bundled_install_start() -> Result {
    let fixture = Fixture::new()?;
    let bytes = b"controlled archive boundary".to_vec();
    let sums = format!("{}  echo-0.2.0-armv7.tar.gz\r\n", digest(&bytes));
    let (base, handle) = server(sums, bytes, 2)?;
    let output = fixture.install().args(["--release-base", &base]).output()?;
    handle.join().map_err(|_| "server panicked")??;
    assert_eq!(output.status.code(), Some(0));
    let result = private_output(&output)?;
    assert_eq!(result["version"], "0.2.0");
    assert_eq!(result["setup"], "complete");
    let ledger = fixture.ledger()?;
    assert_eq!(ledger[0], ["devices"]);
    let dispatch: Vec<_> = ledger
        .iter()
        .filter_map(|args| args.last())
        .filter(|arg| arg.contains("--device") && !arg.starts_with("sh -s -- --device setup-"))
        .collect();
    assert_eq!(dispatch[0], "sh -s -- --device stop");
    assert!(dispatch[1].contains("--device prepare-archive"));
    assert!(dispatch[2].contains("/bundle/scripts/echo-install.sh' --device install --bundle"));
    let start = dispatch
        .iter()
        .position(|call| call.contains("/bundle/scripts/echo-install.sh' --device start"))
        .ok_or("missing bundled start")?;
    assert_eq!(dispatch[start - 1], "sh -s -- --device ensure-signin");
    assert_eq!(dispatch[start + 1], "sh -s -- --device status");
    assert_eq!(
        dispatch
            .iter()
            .filter(|call| call.as_str() == "sh -s -- --device link")
            .count(),
        1
    );
    assert!(
        dispatch
            .last()
            .is_some_and(|call| call.starts_with("sh -s -- --device confirm-check '"))
    );
    assert!(ledger.iter().any(|args| args.get(2).is_some_and(|arg| arg == "push")));
    assert_eq!(fs::read_to_string(fixture.0.join("process-user"))?, "nobody");
    Ok(())
}
#[test]
fn t3_tamper_and_invalid_selected_checksums_call_no_adb() -> Result {
    let valid = format!("{}  echo-0.2.0-armv7.tar.gz\n", digest(b"original"));
    for (sums, count, error) in [
        (valid.clone(), 2, "checksum_mismatch"),
        (valid.repeat(2), 1, "invalid_checksum"),
        ("bad  echo-0.2.0-armv7.tar.gz\n".to_owned(), 1, "invalid_checksum"),
        (String::new(), 1, "invalid_checksum"),
    ] {
        let fixture = Fixture::new()?;
        let (base, handle) = server(sums, b"tampered".to_vec(), count)?;
        let output = fixture.install().args(["--release-base", &base]).output()?;
        handle.join().map_err(|_| "server panicked")??;
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(private_output(&output)?["error"], error);
        assert_eq!(fixture.ledger()?.len(), 0);
    }
    Ok(())
}
#[test]
fn t3_offline_manifest_mismatch_and_missing_member_call_no_adb() -> Result {
    for missing in [false, true] {
        let fixture = Fixture::new()?;
        let bundle = fixture.bundle()?;
        if missing {
            fs::remove_file(bundle.join("bin/echo-bridge"))?;
        } else {
            fs::write(bundle.join("bin/echo-bridge"), b"tamper")?;
        }
        let output = fixture.install().arg("--from-dir").arg(bundle).output()?;
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(private_output(&output)?["error"], "manifest_mismatch");
        assert_eq!(fixture.ledger()?.len(), 0);
    }
    Ok(())
}
#[test]
fn t3_device_archive_refusal_cleans_stage_without_running_bundled_script() -> Result {
    let fixture = Fixture::new()?;
    fs::write(fixture.0.join("unsafe-archive"), b"")?;
    let bytes = b"controlled invalid archive".to_vec();
    let (base, handle) = server(format!("{}  echo-0.2.0-armv7.tar.gz\n", digest(&bytes)), bytes, 2)?;
    let output = fixture.install().args(["--release-base", &base]).output()?;
    handle.join().map_err(|_| "server panicked")??;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        private_output(&output)?["error"],
        "unsafe_archive_or_missing_device_tools"
    );
    let ledger = fixture.ledger()?;
    assert!(
        !ledger
            .iter()
            .flat_map(|args| args.iter())
            .any(|arg| arg.contains("/bundle/scripts/echo-install.sh"))
    );
    assert!(
        ledger
            .last()
            .and_then(|args| args.last())
            .is_some_and(|arg| arg.starts_with("rm -rf '/data/local/tmp/echo-setup-"))
    );
    Ok(())
}
#[test]
fn t3_text_failures_have_fixed_guidance_and_no_external_data() -> Result {
    let fixture = Fixture::new()?;
    let bundle = fixture.bundle()?;
    fs::write(fixture.0.join("no-hassmic"), b"")?;
    let output = fixture
        .command()
        .args(["install", "--adb", env!("CARGO_BIN_EXE_echo-setup-test-adb")])
        .arg("--from-dir")
        .arg(bundle)
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stderr)?,
        "echo-setup: hassmic_required. Hassmic must already be installed; install it separately before continuing.\n"
    );
    assert_eq!(output.stdout, b"");
    Ok(())
}
#[test]
fn t3_adb_discovery_uses_closed_path_then_sdk_without_real_adb() -> Result {
    for sdk in [false, true] {
        let fixture = Fixture::new()?;
        let bundle = fixture.bundle()?;
        let directory = fixture.0.join(if sdk { "sdk/platform-tools" } else { "tools" });
        fs::create_dir_all(&directory)?;
        let executable = directory.join(if cfg!(windows) { "adb.exe" } else { "adb" });
        fs::copy(env!("CARGO_BIN_EXE_echo-setup-test-adb"), executable)?;
        let mut command = fixture.command();
        command
            .current_dir(&fixture.0)
            .args(["install", "--json"])
            .arg("--from-dir")
            .arg(bundle)
            .env("PATH", if sdk { fixture.0.join("empty") } else { directory })
            .env("ANDROID_HOME", fixture.0.join("sdk"))
            .env_remove("ANDROID_SDK_ROOT")
            .env("HOME", fixture.0.join("empty"))
            .env("LOCALAPPDATA", fixture.0.join("empty"));
        let output = command.output()?;
        assert_eq!(output.status.code(), Some(0));
        assert_eq!(private_output(&output)?["ok"], true);
        assert_eq!(fixture.ledger()?[0], ["devices"]);
    }
    Ok(())
}
#[test]
fn t3_offline_install_preserves_native_paths_and_exact_device_selection() -> Result {
    let fixture = Fixture::new()?;
    let bundle = fixture.bundle()?;
    fs::write(
        fixture.0.join("devices"),
        "SYNTHETIC-OTHER\tdevice\r\nSYNTHETIC-DEVICE\tdevice\r\n",
    )?;
    let config = fixture.0.join("config ü ' ; $");
    fs::write(&config, b"synthetic private config")?;
    let output = fixture
        .install()
        .arg("--from-dir")
        .arg(&bundle)
        .arg("--config")
        .arg(&config)
        .args(["--serial", "SYNTHETIC-DEVICE"])
        .output()?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(private_output(&output)?["version"], "0.2.0");
    let ledger = fixture.ledger()?;
    assert!(
        ledger
            .iter()
            .skip(1)
            .all(|args| args[..2] == ["-s", "SYNTHETIC-DEVICE"])
    );
    let config_push = ledger
        .iter()
        .find(|args| args.get(3).is_some_and(|arg| arg == &config.to_string_lossy()))
        .ok_or("config push absent")?;
    assert_eq!(config_push.len(), 5);
    assert!(config_push[4].ends_with("/config"));
    let bridge = bundle.canonicalize()?.join("bin/echo-bridge");
    assert!(
        ledger
            .iter()
            .any(|args| args.get(3).is_some_and(|arg| arg == &bridge.to_string_lossy()))
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn t3_direct_shell_archive_safety_and_install() -> Result {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("python3")
        .current_dir(&root)
        .args(["tests/test_echo_install.py", "--device-transport"])
        .output()?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)?,
        "echo-install direct transport: install=PASS archive-safety=PASS\n"
    );
    Ok(())
}
#[test]
fn t3_prerequisites_refuse_without_push_and_never_print_external_data() -> Result {
    for devices in [
        "",
        "SYNTHETIC-DEVICE\toffline\n",
        "SYNTHETIC-DEVICE\tunauthorized\n",
        "SYNTHETIC-DEVICE\tdevice\nSYNTHETIC-OTHER\tdevice\n",
    ] {
        let fixture = Fixture::new()?;
        let bundle = fixture.bundle()?;
        fs::write(fixture.0.join("devices"), devices)?;
        let output = fixture.install().arg("--from-dir").arg(bundle).output()?;
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(private_output(&output)?["ok"], false);
        assert_eq!(fixture.ledger()?.len(), 1);
    }
    let fixture = Fixture::new()?;
    let bundle = fixture.bundle()?;
    fs::write(fixture.0.join("no-hassmic"), b"")?;
    let output = fixture.install().arg("--from-dir").arg(bundle).output()?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(private_output(&output)?["error"], "hassmic_required");
    assert_eq!(fixture.ledger()?.len(), 2);
    Ok(())
}
#[test]
fn t3_update_verifies_before_stop_and_dispatches_shell_install_start() -> Result {
    let fixture = Fixture::new()?;
    let bundle = fixture.bundle()?;
    let output = Command::new(env!("CARGO_BIN_EXE_echo-setup"))
        .env("ECHO_SETUP_TEST_ROOT", &fixture.0)
        .args([
            "update",
            "--json",
            "--adb",
            env!("CARGO_BIN_EXE_echo-setup-test-adb"),
            "--from-dir",
        ])
        .arg(&bundle)
        .output()?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(private_output(&output)?["command"], "update");
    let ledger = fixture.ledger()?;
    let commands: Vec<_> = ledger
        .iter()
        .filter_map(|args| args.last())
        .filter(|arg| arg.contains("--device") && !arg.starts_with("sh -s -- --device setup-"))
        .collect();
    assert_eq!(commands[0], "sh -s -- --device stop");
    assert!(commands[1].contains("--release-version '0.2.0'"));
    let start = commands
        .iter()
        .position(|call| call.ends_with("--device start"))
        .ok_or("missing bundled start")?;
    assert_eq!(commands[start - 1], "sh -s -- --device ensure-signin");
    assert_eq!(commands[start + 1], "sh -s -- --device status");
    fs::remove_file(fixture.0.join("ledger"))?;
    fs::write(bundle.join("bin/echo-bridge"), "tampered")?;
    let output = Command::new(env!("CARGO_BIN_EXE_echo-setup"))
        .env("ECHO_SETUP_TEST_ROOT", &fixture.0)
        .args([
            "update",
            "--json",
            "--adb",
            env!("CARGO_BIN_EXE_echo-setup-test-adb"),
            "--from-dir",
        ])
        .arg(&bundle)
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(private_output(&output)?["error"], "manifest_mismatch");
    assert_eq!(fixture.ledger()?, Vec::<Vec<String>>::new());
    Ok(())
}
#[test]
fn t3_install_and_update_require_observed_identity_and_matching_release_after_start() -> Result {
    let healthy = "version_current=0.2.0\nversion_previous=null\nprocess=running\nprocess_user=nobody\nuptime_seconds=1\nhassmic_link=unavailable\n";
    for command in ["install", "update"] {
        for (raw, refused, error) in [
            (healthy.replace("nobody", "9999"), false, None),
            (healthy.replace("nobody", "root"), false, Some("unsafe_process_user")),
            (healthy.replace("nobody", "0"), true, Some("unsafe_process_user")),
            (
                healthy.replace("nobody", "SYNTHETIC-ACCOUNT"),
                false,
                Some("unsafe_process_user"),
            ),
            (
                healthy.replace("process=running", "process=stopped"),
                false,
                Some("start_failed"),
            ),
            (healthy.replace("0.2.0", "0.1.0"), false, Some("start_failed")),
            (healthy.replace("0.2.0", "null"), false, Some("start_failed")),
            (
                healthy.replace("version_current=0.2.0\nversion_previous=null\n", ""),
                false,
                Some("start_failed"),
            ),
            ("SYNTHETIC-TOKEN".to_owned(), false, Some("start_failed")),
            (healthy.to_owned(), true, Some("start_failed")),
        ] {
            let fixture = Fixture::new()?;
            let bundle = fixture.bundle()?;
            fs::write(fixture.0.join("status"), raw.replace('\n', "\r\n"))?;
            if refused {
                fs::write(fixture.0.join("status-refused"), b"")?;
            }
            let output = fixture
                .command()
                .args([
                    command,
                    "--json",
                    "--adb",
                    env!("CARGO_BIN_EXE_echo-setup-test-adb"),
                    "--from-dir",
                ])
                .arg(bundle)
                .output()?;
            assert_eq!(output.status.code(), Some(i32::from(error.is_some())));
            let result = private_output(&output)?;
            assert_eq!(result["ok"], error.is_none());
            if let Some(error) = error {
                assert_eq!(result["error"], error);
                assert!(result["setup"].is_null());
            }
            assert_started_then_observed_then_cleaned(&fixture.ledger()?);
        }
    }
    Ok(())
}

fn assert_started_then_observed_then_cleaned(ledger: &[Vec<String>]) {
    let commands: Vec<_> = ledger.iter().filter_map(|args| args.last()).collect();
    let end = commands.len();
    let start = commands.iter().position(|call| call.ends_with("--device start"));
    assert!(start.is_some_and(|index| commands[index + 1] == "sh -s -- --device status"));
    assert!(commands[end - 1].starts_with("rm -rf '/data/local/tmp/echo-setup-"));
    assert!(
        !commands
            .iter()
            .any(|arg| arg.contains("kill ") || arg.ends_with("--device uninstall"))
    );
}
