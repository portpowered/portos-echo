use echo_bridge::device_wifi::relay;
use echo_contracts::secret::Secret;
use serde_json::{Value, json};
use std::{
    error::Error,
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

type Result = std::result::Result<(), Box<dyn Error>>;
const PAIRING: &str = "synthetic-wifi-pair";
const FIXTURE: &str = r#"{
    "status":{"ssid":"Fixture A","reachable":true},
    "scan":{"networks":[
        {"ssid":"Fixture A","signalDbm":-64,"secured":true,"networkId":7},
        {"ssid":"Fixture B","signalDbm":-42,"secured":true,"networkId":null}
    ]}
}"#;

#[tokio::test]
async fn t3_device_wifi_and_legacy_wyoming_run_concurrently() -> Result {
    let state = tempfile::tempdir()?;
    let _device = Device::launch(state.path())?;
    let mut voice = BufReader::new(UnixStream::connect(state.path().join("voice.sock"))?);
    voice.get_ref().set_read_timeout(Some(Duration::from_secs(5)))?;
    voice_command(&mut voice, "describe", "info")?;
    let (_tx, rx) = tokio::sync::mpsc::channel(1);
    let wifi = state.path().join("wifi.sock");
    let mut screen = echo_screen_server::serve_with_wifi(
        "127.0.0.1:0".parse()?,
        Secret::new(PAIRING.into()),
        rx,
        None,
        false,
        Some(relay(Some(&wifi))),
    )
    .await?;
    let client = reqwest::Client::builder().no_proxy().build()?;
    let origin = format!("http://{}", screen.bound_addr);
    for (op, response) in [
        ("status", json!({"ssid":"Fixture A","reachable":true})),
        (
            "scan",
            json!({"networks":[
                {"ssid":"Fixture B","signalDbm":-42,"secured":true,"networkId":null},
                {"ssid":"Fixture A","signalDbm":-64,"secured":true,"networkId":7},
            ]}),
        ),
    ] {
        let mut expected = response;
        expected["ok"] = json!(true);
        expected["op"] = json!(op);
        expected["version"] = json!(1);
        let reply = client
            .post(format!("{origin}/_screen/wifi?token={PAIRING}"))
            .header("origin", &origin)
            .json(&json!({"type":"wifi-request","data":{"version":1,"op":op}}))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        assert_eq!(reply, json!({"type":"wifi-result","data":expected}));
        voice_command(&mut voice, "ping", "pong")?;
    }
    screen.shutdown().await?;
    Ok(())
}

struct Device(Child);

impl Device {
    fn launch(root: &Path) -> std::result::Result<Self, Box<dyn Error>> {
        fs::write(root.join("radio.json"), FIXTURE)?;
        fs::write(root.join("addrs"), "127.0.0.1\n")?;
        let mut device = Self(
            Command::new(env!("CARGO_BIN_EXE_echo-bridge"))
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
                .stderr(Stdio::piped())
                .spawn()?,
        );
        let stderr = device.0.stderr.take().ok_or("missing stderr")?;
        let (ready, receive) = mpsc::channel();
        std::thread::spawn(move || {
            for msg in BufReader::new(stderr).lines().map_while(std::result::Result::ok) {
                if msg.contains("wyoming on Unix socket") {
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

fn voice_command(socket: &mut BufReader<UnixStream>, command: &str, expected: &str) -> Result {
    writeln!(socket.get_mut(), "{}", json!({"type":command}))?;
    loop {
        let mut line = String::new();
        if socket.read_line(&mut line)? == 0 {
            return Err("device voice stream closed".into());
        }
        let header: Value = serde_json::from_str(&line)?;
        if let Some(count) = header["data_length"].as_u64() {
            use std::io::Read;
            socket.read_exact(&mut vec![0; usize::try_from(count)?])?;
        }
        match header["type"].as_str() {
            Some(kind) if kind == expected => return Ok(()),
            Some("hassmic-speaker") => (),
            _ => return Err("unexpected legacy voice reply".into()),
        }
    }
}
