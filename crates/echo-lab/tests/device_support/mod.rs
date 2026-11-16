//! T3 device bench: only loopback tools and the real prebuilt bridge/host.
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Command, Stdio},
    thread::JoinHandle,
};

const FAKE: &str = env!("CARGO_BIN_EXE_echo-lab-fake-run");

struct Bench {
    root: tempfile::TempDir,
    control: std::net::SocketAddr,
    server: Option<JoinHandle<()>>,
}

impl Bench {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut host = Command::new(if cfg!(windows) { "python" } else { "python3" })
            .arg("-u")
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lab_run/sim/host.py"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut input = host.stdin.take().unwrap();
        let mut output = BufReader::new(host.stdout.take().unwrap());
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["event"], "host.ready");
        let address: std::net::SocketAddr = ready["address"].as_str().unwrap().parse().unwrap();
        let control = TcpListener::bind("127.0.0.1:0").unwrap();
        let socket = control.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for stream in control.incoming() {
                let mut stream = stream.unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let value: Value = serde_json::from_str(&line).unwrap();
                if value["stop"] == true {
                    break;
                }
                for command in [
                    json!({"action":"registered"}),
                    json!({"action":"say","pcm":value["pcm"]}),
                ] {
                    writeln!(input, "{command}").unwrap();
                    input.flush().unwrap();
                    line.clear();
                    output.read_line(&mut line).unwrap();
                    assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["event"], "host.ack");
                }
                writeln!(stream, "ok").unwrap();
            }
            drop(input);
            assert!(host.wait().unwrap().success());
        });
        prepare(root.path(), address);
        let bench = Self {
            root,
            control: socket,
            server: Some(server),
        };
        bench.scenario(&json!([
            {"expect_screen":{"state":"ready","message":"Echo Live ready"}},
            {"say":"fixture.pcm"},
            {"expect_trace":["wyoming.run_started","wyoming.mic_frames {count_gte: 10}","wyoming.run_ended"]}
        ]));
        bench
    }

    fn scenario(&self, steps: &Value) {
        fs::write(
            self.root.path().join("scenario.yaml"),
            json!({"id":"m1-hassmic-bridge","milestone":"M1",
            "tiers":["T3","T5"],"setup":{},"steps":steps,"budgets":{},"negative":[]})
            .to_string(),
        )
        .unwrap();
    }

    fn command(&self, bridge: &std::path::Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_echo-lab"));
        command
            .current_dir(self.root.path())
            .env("ECHO_LAB_RUN_FIXTURE", self.root.path().join("fixture.json"))
            .env("ECHO_LAB_RUN_LEDGER", self.root.path().join("ledger.jsonl"))
            .env("ECHO_LAB_DEVICE_CONTROL", self.control.to_string())
            .args([
                "run",
                "scenario.yaml",
                "--driver",
                "device",
                "--bench",
                "ring",
                "--ring",
                "fixture-ring",
                "--port",
                FAKE,
                "--adb",
                FAKE,
                "--stt",
                FAKE,
                "--python",
                FAKE,
                "--out",
                "artifacts",
                "--bridge",
            ])
            .arg(bridge)
            .args(["--bridge-config", "./bridge.toml"]);
        command
    }

    fn report(&self) -> Value {
        serde_json::from_slice(&fs::read(self.root.path().join("artifacts/report.json")).unwrap()).unwrap()
    }
}

fn prepare(root: &std::path::Path, address: std::net::SocketAddr) {
    let template = include_str!("../../../../examples/bridge.example.toml")
        .replace("echo.example.invalid", "127.0.0.1")
        .replace("wyoming_port = 16700", &format!("wyoming_port = {}", address.port()))
        .replace("0.0.0.0:16900", "127.0.0.1:0"); // lint-rules: allow-literal inert template substitution
    fs::write(root.join("bridge.toml"), template).unwrap();
    fs::write(root.join("fixture.json"), json!({"real_config":true}).to_string()).unwrap();
    fs::write(
        root.join("fixture.pcm"),
        include_bytes!("../../../../tests/fixtures/lab/w1-utterance.pcm"),
    )
    .unwrap();
}

impl Drop for Bench {
    fn drop(&mut self) {
        if let Ok(mut stream) = TcpStream::connect(self.control) {
            let _ = writeln!(stream, "{{\"stop\":true}}");
        }
        if let Some(server) = self.server.take() {
            server.join().unwrap();
        }
    }
}

fn bridge() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_echo-lab")).with_file_name(if cfg!(windows) {
        "echo-bridge.exe"
    } else {
        "echo-bridge"
    })
}

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn device_m1_prefix_ready_and_ordered_complete_wake() {
    let bench = Bench::new();
    let result = bench.command(&bridge()).output().unwrap();
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report = bench.report();
    assert_eq!(report["result"], "pass");
    assert_eq!(report["driver"], "device");
    assert_eq!(
        report["checks"],
        json!({"step_0":true,"step_1":true,"step_2":true,
        "bridge_errors_absent":true,"session_released":true})
    );
    let observations = report["observations"].as_array().unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0]["run_id"], "run-1");
    assert!(observations[0]["count"].as_u64().unwrap() >= 10);
    let root = bench.root.path().join("artifacts");
    assert!(!root.join("configured.private").exists());
    assert!(!root.join("bridge.private.jsonl").exists());
    assert!(!root.join("utterance-1.wav").exists());
    let ui: Value = serde_json::from_str(fs::read_to_string(root.join("device-steps.jsonl")).unwrap().trim()).unwrap();
    assert_eq!(ui["state"], json!({"state":"ready","message":"Echo Live ready"}));
    assert_eq!(ui["seq"], 1);
    assert_eq!(report["evidence"], json!(["events.jsonl", "device-steps.jsonl"]));
}

#[test]
#[ignore = "T3 requires pinned hassmic-host"]
fn device_m1_startup_failure_collects_private_inputs_without_playback() {
    let bench = Bench::new();
    let result = bench.command(std::path::Path::new(FAKE)).output().unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(bench.report()["checks"]["step_0"], false);
    // No final trace exists after this startup fault, so cleanup must not
    // claim a complete observation even though the owned processes are reaped.
    assert_eq!(bench.report()["checks"]["session_released"], false);
    assert_eq!(bench.report()["result"], "fail");
    assert!(!bench.root.path().join("artifacts/configured.private").exists());
    let ledger = fs::read_to_string(bench.root.path().join("ledger.jsonl")).unwrap();
    let operations: Vec<Value> = ledger.lines().map(|s| serde_json::from_str(s).unwrap()).collect();
    assert_eq!(operations.iter().filter(|v| v["operation"] == "play").count(), 0);
}
