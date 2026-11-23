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
    let origin = format!("http://127.0.0.1:{}", screen.bound_addr.port());
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

#[tokio::test]
async fn t3_device_wifi_busy_and_disconnected_client_reconcile_once() -> Result {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let state = tempfile::tempdir()?;
    let _device = Device::launch(state.path())?;
    let gate_path = state.path().join("association.sock");
    let gate = tokio::net::UnixListener::bind(&gate_path)?;
    let mut fixture: Value = serde_json::from_str(FIXTURE)?;
    fixture["association_gate"] = json!(gate_path);
    fs::write(state.path().join("radio.json"), fixture.to_string())?;
    let (_tx, rx) = tokio::sync::mpsc::channel(1);
    let path = state.path().join("wifi.sock");
    let mut screen = echo_screen_server::serve_with_wifi(
        "127.0.0.1:0".parse()?,
        Secret::new(PAIRING.into()),
        rx,
        None,
        false,
        Some(relay(Some(&path))),
    )
    .await?;
    let origin = format!("http://127.0.0.1:{}", screen.bound_addr.port());
    let client = reqwest::Client::builder().no_proxy().build()?;
    let input = ["incorrect", "gate"].join("-");
    let join = tokio::spawn(request(
        client.clone(),
        origin.clone(),
        json!({
            "op":"join","ssid":"Fixture B","password":input
        }),
    ));
    let (mut radio, _) = tokio::time::timeout(Duration::from_secs(2), gate.accept()).await??;
    assert_eq!(radio.read_u8().await?, b'J');
    assert_eq!(
        request(client.clone(), origin.clone(), json!({"op":"forget","networkId":7})).await?,
        json!({"version":1,"op":"forget","ok":false,"error":"wifi-busy"})
    );
    join.abort();
    assert!(join.await.unwrap_err().is_cancelled());
    radio.write_all(b"R").await?;
    reconciled(client, origin).await?;
    screen.shutdown().await?;
    Ok(())
}

async fn reconciled(client: reqwest::Client, origin: String) -> Result {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let status = request(client.clone(), origin.clone(), json!({"op":"status"})).await?;
            if status["ok"] == true {
                assert_eq!(
                    status,
                    json!({"version":1,"op":"status","ok":true,"ssid":"Fixture A","reachable":true})
                );
                break;
            }
            assert_eq!(status["error"], "wifi-unavailable");
            tokio::task::yield_now().await;
        }
        let scan = request(client, origin, json!({"op":"scan"})).await?;
        assert_eq!(scan["networks"][0]["networkId"], Value::Null);
        assert_eq!(scan["networks"][1]["networkId"], 7);
        Ok::<_, reqwest::Error>(())
    })
    .await??;
    Ok(())
}

async fn request(
    client: reqwest::Client,
    origin: String,
    mut data: Value,
) -> std::result::Result<Value, reqwest::Error> {
    data["version"] = json!(1);
    let response: Value = client
        .post(format!("{origin}/_screen/wifi?token={PAIRING}"))
        .header("origin", &origin)
        .json(&json!({"type":"wifi-request","data":data}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(response["type"], "wifi-result");
    Ok(response["data"].clone())
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
