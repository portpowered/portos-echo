use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
mod failures;
mod suite;
pub struct Session(pub PathBuf);

impl Session {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "lab sim {}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        // The name carries this process id; one that already exists is a stale leftover of an
        // interrupted earlier process that had the same id (Windows reuses ids), never a live one.
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    pub fn scenario(&self, steps: Value, budgets: Value) {
        let mut scenario = json!({"id":"controlled","milestone":"W1","tiers":["T3"], "setup":{},"negative":[]});
        scenario["steps"] = steps;
        scenario["budgets"] = budgets;
        fs::write(self.0.join("scenario.yaml"), scenario.to_string()).unwrap();
        fs::write(self.0.join("fixture.pcm"), [1_u8, 0, 2, 0]).unwrap();
    }

    pub fn command(&self, bridge: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_echo-lab"));
        command
            .current_dir(&self.0)
            .args([
                "run",
                "scenario.yaml",
                "--driver",
                "sim",
                "--bridge",
                bridge,
                "--out",
                "artifacts",
            ])
            .env("ECHO_LAB_SIM_SCRIPT", self.0.join("script.json"))
            .env("ECHO_LAB_SIM_ENDPOINTS", self.0.join("endpoints.json"))
            .env("ECHO_LAB_SIM_CANCEL", self.0.join("cancel"));
        command
    }

    pub fn script(&self, script: &Value) -> Output {
        fs::write(self.0.join("script.json"), script.to_string()).unwrap();
        self.command(env!("CARGO_BIN_EXE_echo-lab-sim-script"))
            .output()
            .unwrap()
    }

    pub fn report(&self) -> Value {
        let value: Value = serde_json::from_slice(&fs::read(self.0.join("artifacts/report.json")).unwrap()).unwrap();
        let schema: Value = serde_json::from_str(include_str!("../../../../schemas/report.schema.json")).unwrap();
        assert!(jsonschema::validator_for(&schema).unwrap().is_valid(&value));
        assert_eq!(value["driver"], "sim");
        assert_eq!(value["tier"], "T3");
        assert_eq!(value["bench_session_ids"], json!([]));
        assert_eq!(value["device"], Value::Null);
        assert!(value["limitations"].as_array().unwrap().contains(&json!(
            "pinned hassmic-host binds wildcard listeners (upstream; no bind-address option)"
        )));
        assert_eq!(value["hassmic_commit"], "f68e39537420fdae8805795d8950e1daab35a203");
        assert!(!self.0.join("artifacts/bridge.private.jsonl").exists());
        assert!(!self.0.join("artifacts/ui.private.json").exists());
        value
    }

    pub fn endpoints(&self) -> Vec<std::net::SocketAddr> {
        let endpoints = fs::read_to_string(self.0.join("artifacts/events.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|v| v["event"] == "sim.endpoint")
            .map(|v| v["address"].as_str().unwrap().parse::<std::net::SocketAddr>().unwrap())
            .collect::<Vec<_>>();
        if self.0.join("endpoints.json").is_file() {
            let actual: Vec<std::net::SocketAddr> =
                serde_json::from_slice(&fs::read(self.0.join("endpoints.json")).unwrap()).unwrap();
            assert_eq!(endpoints, actual);
        }
        assert_eq!(endpoints.len(), 3);
        for (index, endpoint) in endpoints.iter().enumerate() {
            assert_eq!(endpoint.ip(), std::net::Ipv4Addr::LOCALHOST);
            if let Ok(mut stream) =
                std::net::TcpStream::connect_timeout(endpoint, std::time::Duration::from_millis(100))
            {
                // Windows may retain WSL's forwarding socket after its Linux
                // target closes. The target must reset/EOF, never respond.
                assert!(cfg!(windows) && index == 0, "listener leaked: {endpoint}");
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                    .unwrap();
                let _ = stream.write_all(b"{\"type\":\"describe\"}\n");
                let result = stream.read(&mut [0]);
                let closed = matches!(result, Ok(0))
                    || result.as_ref().err().is_some_and(|error| {
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                                | std::io::ErrorKind::BrokenPipe
                        )
                    });
                assert!(closed, "host still reachable at {endpoint}: {result:?}");
            }
        }
        endpoints
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn records() -> Value {
    json!([{"event":"wyoming.run_started","elapsed_ms":0,"run_id":"one"},
        {"event":"wyoming.mic_frames","elapsed_ms":1,"run_id":"one","count":10},
        {"event":"wyoming.run_ended","elapsed_ms":2,"run_id":"one"},
        {"event":"live.transcript","elapsed_ms":3,"run_id":"one","text":"TEST echo [a.*]"},
        {"event":"timing","elapsed_ms":4,"run_id":"one","name":"first_audio_after_endpoint_ms","duration_ms":1500}])
}
