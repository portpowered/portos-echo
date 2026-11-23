//! Paired HTTP journey against the production core and a separate device role.
use serde_json::{Value, json};
use std::{
    fs::{File, Permissions},
    io::{BufRead, BufReader, Write},
    net::SocketAddr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UnixStream},
    process::{Child, Command},
};

pub const ID: &str = "a2-wifi-settings";
const BRIDGE_LOG: &str = "core.log";
const DEVICE_LOG: &str = "device.log";

fn accepted_input() -> String {
    ["join", "Fixture B"].join("-")
}

fn rejected_input() -> String {
    ["incorrect", "42"].join("-")
}

pub fn configure_settings(private: &Path) -> Result<(), String> {
    let mut config = std::fs::File::options()
        .append(true)
        .open(private.join("configured.private/bridge.toml"))
        .map_err(|_| "Wi-Fi: cannot open private core config")?;
    writeln!(
        config,
        "\n[home]\ndir = '{}'\nagent_tools = false",
        private.join("settings").display()
    )
    .map_err(|_| "Wi-Fi: cannot configure paired Settings".into())
}

pub fn record(artifacts: &mut crate::lab_run::Artifacts, action: &str, result: Value) -> Result<(), String> {
    let name = "wifi-results.jsonl";
    let mut file = std::fs::File::options()
        .append(true)
        .create(true)
        .open(artifacts.root.join(name))
        .map_err(|_| "Wi-Fi: cannot open result evidence")?;
    let mut entry = json!({"step":action});
    entry["result"] = result;
    writeln!(file, "{entry}").map_err(|_| "Wi-Fi: cannot record result evidence")?;
    if !artifacts.evidence.iter().any(|entry| entry == name) {
        artifacts.evidence.push(name.to_owned());
    }
    Ok(())
}

pub struct Device {
    child: Child,
    root: tempfile::TempDir,
}

impl Device {
    pub async fn start(bridge: &Path) -> Result<Self, String> {
        let root = tempfile::Builder::new()
            .permissions(Permissions::from_mode(0o700))
            .tempdir()
            .map_err(|_| "Wi-Fi: cannot isolate device")?;
        let path = root.path();
        std::fs::write(path.join("addrs"), "127.0.0.1\n").map_err(|_| "Wi-Fi: cannot write address fixture")?;
        save(
            &path.join("radio.json"),
            &json!({
                "status":{"ssid":"Fixture A","reachable":true},
                "scan":{"networks":[
                    {"ssid":"Fixture B","signalDbm":-42,"secured":true,"networkId":null},
                    {"ssid":"Fixture A","signalDbm":-64,"secured":true,"networkId":7},
                    {"ssid":"Fixture C","signalDbm":-76,"secured":false,"networkId":12}
                ]}
            }),
        )?;
        let log = File::create(path.join(DEVICE_LOG)).map_err(|_| "Wi-Fi: cannot capture device log")?;
        let child = Command::new(bridge)
            .env_clear()
            .env("HASSMIC_STATE", path)
            .env("HASSMIC_FAKE_ADDRS", path.join("addrs"))
            .env("HASSMIC_PLAY", path.join("play.raw"))
            .env("HASSMIC_MDNS_FILE", path.join("no-mdns"))
            .env("ECHO_LAB_WIFI_FIXTURE", path.join("radio.json"))
            .current_dir(path)
            .args(["device", "--wyoming-socket"])
            .arg(path.join("voice.sock"))
            .args(["-P", "wyoming", "-D", "-a", "0", "-w", "remote"])
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|_| "Wi-Fi: cannot capture device stdout")?)
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "Wi-Fi: cannot start device role")?;
        let device = Self { child, root };
        tokio::time::timeout(Duration::from_secs(5), device.ready())
            .await
            .map_err(|_| "Wi-Fi: device socket did not start")?;
        Ok(device)
    }

    async fn ready(&self) {
        let socket = self.socket();
        while UnixStream::connect(&socket).await.is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    pub fn fixture(&self) -> PathBuf {
        self.root.path().join("radio.json")
    }

    pub fn socket(&self) -> PathBuf {
        self.root.path().join("wifi.sock")
    }

    pub fn bridge_log(&self) -> Result<File, String> {
        File::create(self.root.path().join(BRIDGE_LOG)).map_err(|_| "Wi-Fi: cannot capture core log".into())
    }

    pub fn trace(&self) -> PathBuf {
        self.root.path().join("bridge.private.jsonl")
    }

    pub async fn close(mut self) -> Result<usize, String> {
        self.child
            .kill()
            .await
            .map_err(|_| "Wi-Fi: cannot collect device role")?;
        let inputs = [accepted_input(), rejected_input()];
        [
            self.trace(),
            self.root.path().join(BRIDGE_LOG),
            self.root.path().join(DEVICE_LOG),
        ]
        .iter()
        .try_fold(0, |total, path| count_matches(path, &inputs).map(|count| total + count))
    }

    pub async fn perform(&self, action: &str, screen: SocketAddr) -> Result<Value, String> {
        match action {
            "wifi-reject" => rejected(screen).await,
            "wifi-lose" => self.lose(screen).await,
            "wifi-join" => joined(screen).await,
            "wifi-forget" => forgotten(screen).await,
            _ => Err("Wi-Fi: unsupported scenario operation".into()),
        }
    }

    async fn lose(&self, screen: SocketAddr) -> Result<Value, String> {
        let path = self.fixture();
        let mut radio: Value = serde_json::from_slice(&std::fs::read(&path).map_err(|_| "Wi-Fi: radio missing")?)
            .map_err(|_| "Wi-Fi: malformed radio state")?;
        radio["status"] = json!({"ssid":null,"reachable":false});
        save(&path, &radio)?;
        expect(
            screen,
            json!({"op":"status"}),
            json!({"ssid":null,"reachable":false,"ok":true}),
        )
        .await
    }
}

fn count_matches(path: &Path, inputs: &[String]) -> Result<usize, String> {
    let file = File::open(path).map_err(|_| "Wi-Fi: missing private trace or log capture")?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut matches = 0;
    while reader
        .read_until(b'\n', &mut line)
        .map_err(|_| "Wi-Fi: cannot read private capture")?
        != 0
    {
        for input in inputs {
            matches += line
                .windows(input.len())
                .filter(|bytes| *bytes == input.as_bytes())
                .count();
        }
        line.clear();
    }
    Ok(matches)
}

pub fn action(value: &str) -> bool {
    matches!(value, "wifi-reject" | "wifi-lose" | "wifi-join" | "wifi-forget")
}

fn save(path: &Path, radio: &Value) -> Result<(), String> {
    let next = path.with_extension("scenario-next");
    std::fs::write(&next, radio.to_string())
        .and_then(|()| std::fs::rename(next, path))
        .map_err(|_| "Wi-Fi: cannot set radio state".into())
}

async fn joined(addr: SocketAddr) -> Result<Value, String> {
    let input = accepted_input();
    let reply = expect(
        addr,
        json!({"op":"join","ssid":"Fixture B","password":input}),
        json!({"ok":true,"ssid":"Fixture B","reachable":true}),
    )
    .await?;
    expect(
        addr,
        json!({"op":"status"}),
        json!({"ok":true,"ssid":"Fixture B","reachable":true}),
    )
    .await?;
    Ok(reply)
}

async fn rejected(addr: SocketAddr) -> Result<Value, String> {
    let input = rejected_input();
    let reply = expect(
        addr,
        json!({"op":"join","ssid":"Fixture B","password":input}),
        json!({"ok":false,"error":"wifi-join-failed","ssid":"Fixture A","reachable":true}),
    )
    .await?;
    let rows = request(addr, json!({"op":"scan"})).await?;
    if rows["networks"][0]["ssid"] != "Fixture B" || !rows["networks"][0]["networkId"].is_null() {
        return Err("Wi-Fi: failed candidate became saved".into());
    }
    Ok(reply)
}

async fn forgotten(addr: SocketAddr) -> Result<Value, String> {
    let reply = expect(
        addr,
        json!({"op":"forget","networkId":7}),
        json!({"ok":true,"ssid":"Fixture B","reachable":true}),
    )
    .await?;
    expect(
        addr,
        json!({"op":"forget","networkId":7}),
        json!({"ok":false,"error":"wifi-not-saved"}),
    )
    .await?;
    let rows = request(addr, json!({"op":"scan"})).await?;
    if rows["networks"][1]["ssid"] != "Fixture A"
        || !rows["networks"][1]["networkId"].is_null()
        || rows["networks"][2]["ssid"] != "Fixture C"
        || rows["networks"][2]["networkId"] != 12
    {
        return Err("Wi-Fi: forget changed the wrong saved profile".into());
    }
    Ok(reply)
}

async fn expect(addr: SocketAddr, frame: Value, expected: Value) -> Result<Value, String> {
    let op = frame["op"].clone();
    let reply = request(addr, frame).await?;
    let mut expected = expected;
    expected["op"] = op;
    expected["version"] = json!(1);
    if reply != expected {
        return Err("Wi-Fi: unexpected result or reconciled state".into());
    }
    Ok(reply)
}

async fn request(addr: SocketAddr, mut data: Value) -> Result<Value, String> {
    data["version"] = json!(1);
    let mut frame = json!({"type":"wifi-request"});
    frame["data"] = data;
    let body = frame.to_string();
    let headers = format!(
        "POST /_screen/wifi?token=synthetic-local-screen HTTP/1.1\r\nHost: {addr}\r\n\
        Origin: http://{addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut socket = TcpStream::connect(addr)
        .await
        .map_err(|_| "Wi-Fi: cannot connect to paired core")?;
    socket
        .write_all(headers.as_bytes())
        .await
        .map_err(|_| "Wi-Fi: cannot send paired headers")?;
    socket
        .write_all(body.as_bytes())
        .await
        .map_err(|_| "Wi-Fi: cannot send paired request")?;
    let mut bytes = Vec::new();
    socket
        .take(70_000)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| "Wi-Fi: cannot read result")?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "Wi-Fi: invalid HTTP result")?;
    let (headers, body) = text.split_once("\r\n\r\n").ok_or("Wi-Fi: incomplete HTTP result")?;
    if !headers.starts_with("HTTP/1.1 200 ") {
        let status = headers
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or("Wi-Fi: malformed HTTP status")?;
        return Err(format!("Wi-Fi: paired request returned HTTP {status}"));
    }
    let response: Value = serde_json::from_str(body).map_err(|_| "Wi-Fi: invalid v1 result")?;
    if response["type"] != "wifi-result" || response["data"]["version"] != 1 {
        return Err("Wi-Fi: unsupported result version".into());
    }
    Ok(response["data"].clone())
}
