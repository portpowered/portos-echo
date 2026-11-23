//! T3 USB command journeys through the Cargo-built recording adb. The Linux
//! journey also uses the prebuilt bridge and the same radio fake as the screen.
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "echo-setup-wifi-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root)?;
        fs::write(root.join("devices"), "SYNTHETIC-DEVICE\tdevice\n")?;
        Ok(Self(root))
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_echo-setup"));
        command
            .env("ECHO_SETUP_TEST_ROOT", &self.0)
            .env_remove("ECHO_SETUP_TEST_WIFI_BRIDGE")
            .arg("wifi")
            .args(args)
            .arg("--adb")
            .arg(env!("CARGO_BIN_EXE_echo-setup-test-adb"))
            .args(["--serial", "SYNTHETIC-DEVICE"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn no_password(&self, output: &Output, password: &str) -> Result {
        let ledger = fs::read(self.0.join("ledger"))?;
        let hits = [&output.stdout, &output.stderr, &ledger]
            .iter()
            .filter(|bytes| String::from_utf8_lossy(bytes).contains(password))
            .count();
        assert_eq!(hits, 0, "USB output or argv contained the test credential");
        Ok(())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn result(output: &Output, code: i32, expected: &Value) -> Result {
    assert_eq!(output.status.code(), Some(code));
    assert_eq!(output.stderr.len(), 0);
    assert_eq!(&serde_json::from_slice::<Value>(&output.stdout)?, expected);
    Ok(())
}

fn report(op: &str, ssid: Option<&str>, reachable: bool, error: Option<&str>) -> Value {
    let mut value = json!({
        "type":"result","command":"wifi","operation":op,"ok":error.is_none(),"ssid":ssid,"reachable":reachable
    });
    if let Some(error) = error {
        value["error"] = json!(error);
    }
    value
}

fn failure(op: &str, error: &str) -> Value {
    json!({"type":"result","command":"wifi","operation":op,"ok":false,"error":error})
}

#[test]
fn t3_wifi_status_and_legacy_binary_absence() -> Result {
    let fixture = Fixture::new()?;
    let output = fixture.0.join("wifi-output");
    fs::write(
        &output,
        "{\"type\":\"wifi-result\",\"data\":{\"version\":1,\"op\":\"status\",\"ok\":true,\"ssid\":\"Fixture A\",\"reachable\":true}}\n",
    )?;
    let status = fixture.command(&["status"]).output()?;
    assert_eq!(status.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(status.stdout)?.replace("\r\n", "\n"),
        "Wi-Fi: Fixture A (reachable)\n"
    );
    assert_eq!(status.stderr.len(), 0);
    fs::write(
        &output,
        "{\"type\":\"wifi-result\",\"data\":{\"version\":1,\"op\":\"status\",\"ok\":true,\"ssid\":null,\"reachable\":false}}\n",
    )?;
    result(
        &fixture.command(&["status", "--json"]).output()?,
        0,
        &report("status", None, false, None),
    )?;
    fs::write(&output, "wifi-unsupported\n")?;
    fs::write(fixture.0.join("wifi-fail"), "")?;
    result(
        &fixture.command(&["status", "--json"]).output()?,
        1,
        &failure("status", "wifi-unsupported"),
    )
}

#[test]
fn t3_wifi_usage_and_missing_input_fail_before_remote_shell() -> Result {
    let fixture = Fixture::new()?;
    for args in [
        &["restart"][..],
        &["status", "--ssid", "Fixture A"],
        &["join", "--ssid"],
        &["join", "--ssid", "Fixture A", "--timeout-seconds", "0"],
    ] {
        assert_eq!(fixture.command(args).output()?.status.code(), Some(2));
    }
    assert!(!fixture.0.join("ledger").exists());
    result(
        &fixture.command(&["join", "--ssid", "Fixture A", "--json"]).output()?,
        1,
        &failure("join", "wifi-input-failed"),
    )?;
    assert_eq!(
        fs::read_to_string(fixture.0.join("ledger"))?.trim_end(),
        "[\"devices\"]"
    );
    Ok(())
}

#[test]
fn t3_wifi_rejects_reflected_child_output_in_both_input_modes() -> Result {
    let fixture = Fixture::new()?;
    fs::write(fixture.0.join("wifi-reflect"), "")?;
    let private = ["literal", "fixture", "input"].join("-");
    let password = fixture.0.join("one-line");
    fs::write(&password, format!("{private}\r\nignored\n"))?;
    let password_path = password.to_str().ok_or("non-UTF-8 fixture path")?;
    let via_file = fixture
        .command(&[
            "join",
            "--ssid",
            "Fixture B",
            "--json",
            "--password-file",
            password_path,
        ])
        .output()?;
    let via_stdin = send(
        &mut fixture.command(&["join", "--ssid", "Fixture B", "--json"]),
        &private,
    )?;
    for output in [via_file, via_stdin] {
        fixture.no_password(&output, &private)?;
        result(&output, 1, &failure("join", "wifi-device-failed"))?;
    }
    Ok(())
}

fn send(command: &mut Command, password: &str) -> Result<Output> {
    let mut child = command.stdin(Stdio::piped()).spawn()?;
    writeln!(child.stdin.take().ok_or("missing stdin")?, "{password}")?;
    Ok(child.wait_with_output()?)
}

#[test]
fn t3_wifi_deadline_covers_waiting_for_input() -> Result {
    let fixture = Fixture::new()?;
    let mut child = fixture
        .command(&["join", "--ssid", "Fixture B", "--timeout-seconds", "1", "--json"])
        .stdin(Stdio::piped())
        .spawn()?;
    let keep_input_open = child.stdin.take().ok_or("missing stdin")?;
    let output = child.wait_with_output()?;
    drop(keep_input_open);
    result(&output, 3, &failure("join", "wifi-timeout"))?;
    assert_eq!(
        fs::read_to_string(fixture.0.join("ledger"))?.trim_end(),
        "[\"devices\"]"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
mod assembled {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        path::Path,
        process::Child,
        sync::mpsc,
        time::Duration,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn bridge() -> Result<PathBuf> {
        Ok(Path::new(env!("CARGO_BIN_EXE_echo-setup"))
            .parent()
            .ok_or("missing Cargo target root")?
            .join("echo-bridge"))
    }

    fn command(fixture: &Fixture, bridge: &Path, args: &[&str]) -> Command {
        let mut command = fixture.command(args);
        command.env("ECHO_SETUP_TEST_WIFI_BRIDGE", bridge);
        command
    }

    struct Device(Child);

    impl Device {
        fn launch(fixture: &Fixture, binary: &Path) -> Result<Self> {
            let root = &fixture.0;
            let radio = json!({
                "status":{"ssid":"Fixture A","reachable":true},
                "scan":{"networks":[
                    {"ssid":"Fixture A","signalDbm":-64,"secured":true,"networkId":7},
                    {"ssid":"Fixture B","signalDbm":-42,"secured":true,"networkId":null}
                ]}
            });
            fs::write(root.join("radio.json"), radio.to_string())?;
            fs::write(root.join("addrs"), "127.0.0.1\n")?;
            let mut device = Self(
                Command::new(binary)
                    .env_clear()
                    .env("HASSMIC_STATE", root)
                    .env("HASSMIC_FAKE_ADDRS", root.join("addrs"))
                    .env("HASSMIC_PLAY", root.join("play.raw"))
                    .env("HASSMIC_MDNS_FILE", root.join("no-mdns"))
                    .env("ECHO_LAB_WIFI_FIXTURE", root.join("radio.json"))
                    .current_dir(root)
                    .args(["device", "--wyoming-socket"])
                    .arg(root.join("voice.sock"))
                    .args(["-P", "wyoming", "-D", "-a", "0", "-w", "remote"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()?,
            );
            let stderr = device.0.stderr.take().ok_or("missing readiness pipe")?;
            let (ready, receive) = mpsc::channel();
            std::thread::spawn(move || {
                for message in BufReader::new(stderr).lines().map_while(std::result::Result::ok) {
                    if message.contains("wyoming on Unix socket") {
                        let _ = ready.send(());
                    }
                }
            });
            receive.recv_timeout(Duration::from_secs(5))?;
            Ok(device)
        }
    }

    impl Drop for Device {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    #[ignore = "T3: requires the bridge binary built by sim-prereqs"]
    fn t3_wifi_usb_recovers_through_device_command_and_shared_radio() -> Result {
        let bridge = bridge()?;
        let fixture = Fixture::new()?;
        let command = |args: &[&str]| command(&fixture, &bridge, args);
        let status = || command(&["status", "--json"]).output();
        result(&status()?, 1, &failure("status", "wifi-unsupported"))?;
        let _device = Device::launch(&fixture, &bridge)?;
        result(&status()?, 0, &report("status", Some("Fixture A"), true, None))?;
        let wrong = ["fixture", "wrong"].join("-");
        let rejected = send(&mut command(&["join", "--ssid", "Fixture B", "--json"]), &wrong)?;
        fixture.no_password(&rejected, &wrong)?;
        result(
            &rejected,
            1,
            &report("join", Some("Fixture A"), true, Some("wifi-join-failed")),
        )?;
        result(&status()?, 0, &report("status", Some("Fixture A"), true, None))?;
        for (ssid, file) in [("Fixture B", true), ("Fixture A", false)] {
            let private = format!("join-{ssid}");
            let mut join = command(&["join", "--ssid", ssid, "--json"]);
            let joined = if file {
                let path = fixture.0.join("private-input");
                fs::write(&path, format!("{private}\r\nunused\n"))?;
                join.args(["--password-file"]).arg(path).output()?
            } else {
                send(&mut join, &private)?
            };
            fixture.no_password(&joined, &private)?;
            result(&joined, 0, &report("join", Some(ssid), true, None))?;
            result(&status()?, 0, &report("status", Some(ssid), true, None))?;
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "T3: requires the bridge binary built by sim-prereqs"]
    async fn t3_wifi_usb_timeout_leaves_admitted_join_to_reconcile() -> Result {
        let bridge = bridge()?;
        let fixture = Fixture::new()?;
        let _device = Device::launch(&fixture, &bridge)?;
        let gate_path = fixture.0.join("association.sock");
        let gate = tokio::net::UnixListener::bind(&gate_path)?;
        let radio_path = fixture.0.join("radio.json");
        let mut radio: Value = serde_json::from_slice(&fs::read(&radio_path)?)?;
        radio["association_gate"] = json!(gate_path);
        fs::write(radio_path, radio.to_string())?;
        let args = ["join", "--ssid", "Fixture B", "--timeout-seconds", "2", "--json"];
        let mut child = tokio::process::Command::from(command(&fixture, &bridge, &args))
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .spawn()?;
        let password = ["fixture", "wrong"].join("-");
        let mut stdin = child.stdin.take().ok_or("missing stdin")?;
        stdin.write_all(format!("{password}\n").as_bytes()).await?;
        drop(stdin);
        let (mut radio, _) = tokio::time::timeout(Duration::from_secs(3), gate.accept()).await??;
        assert_eq!(radio.read_u8().await?, b'J');
        let output = child.wait_with_output().await?;
        fixture.no_password(&output, &password)?;
        result(&output, 3, &failure("join", "wifi-timeout"))?;
        radio.write_all(b"R").await?;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let output = tokio::process::Command::from(command(&fixture, &bridge, &["status", "--json"]))
                    .output()
                    .await?;
                if output.status.success() {
                    return result(&output, 0, &report("status", Some("Fixture A"), true, None));
                }
                result(&output, 1, &failure("status", "wifi-unavailable"))?;
                tokio::task::yield_now().await;
            }
        })
        .await?
    }
}
