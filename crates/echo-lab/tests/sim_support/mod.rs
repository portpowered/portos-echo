use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
mod cli_owner;
mod emotion;
mod failures;
mod host_owner;
mod mcp;
mod production;
mod suite;
pub struct Session(pub PathBuf, host_owner::Host);

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
        let path = path.canonicalize().unwrap();
        cli_owner::prepare(&path);
        Self(path, host_owner::Host::new())
    }

    pub fn scenario(&self, steps: Value, budgets: Value) {
        let mut scenario = json!({"id":"controlled","milestone":"W1","tiers":["T3"], "setup":{},"negative":[]});
        scenario["steps"] = steps;
        scenario["budgets"] = budgets;
        fs::write(self.0.join("scenario.yaml"), scenario.to_string()).unwrap();
        fs::write(self.0.join("fixture.pcm"), [1_u8, 0, 2, 0]).unwrap();
    }

    pub fn cli(&self) -> Command {
        let mut command = Command::new(cli_owner::cli(&self.0));
        command
            .current_dir(&self.0)
            .args(["run", "--python", env!("CARGO_BIN_EXE_echo-lab-sim-script")])
            .env(
                "ECHO_LAB_TEST_HOST_DIRECTORY",
                std::path::Path::new(self.1.python())
                    .parent()
                    .unwrap()
                    .file_name()
                    .unwrap(),
            );
        command
    }

    pub fn command(&self, bridge: &str) -> Command {
        let mut command = self.cli();
        let bridge = cli_owner::selected(&self.0, bridge);
        command
            .args([
                "scenario.yaml",
                "--driver",
                "sim",
                "--bridge",
                &bridge,
                "--out",
                "artifacts",
            ])
            .env("ECHO_LAB_SIM_SCRIPT", self.0.join("script.json"))
            .env("ECHO_LAB_SIM_ENDPOINTS", self.0.join("endpoints.json"))
            .env("ECHO_LAB_SIM_CANCEL", self.0.join("cancel"));
        command
    }

    // M5 retains the real direct child, including cancellation of pending link.
    pub fn m5_command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_echo-lab"));
        command
            .current_dir(&self.0)
            .args([
                "run",
                "scenario.yaml",
                "--driver",
                "sim",
                "--out",
                "artifacts",
                "--python",
                env!("CARGO_BIN_EXE_echo-lab-sim-script"),
            ])
            .env("ECHO_LAB_TEST_RUNTIME_OWNERS", self.0.join("runtime-owners.json"))
            .env(
                "ECHO_LAB_TEST_HOST_DIRECTORY",
                std::path::Path::new(self.1.python())
                    .parent()
                    .unwrap()
                    .file_name()
                    .unwrap(),
            );
        command
    }

    pub fn m5_owners_closed(&self, generations: usize) {
        let observations: Value =
            serde_json::from_slice(&fs::read(self.0.join("runtime-owners.json")).unwrap()).unwrap();
        let owners = observations.as_array().unwrap();
        assert_eq!(
            owners.len(),
            generations,
            "every registered bridge generation needs its own owner handle"
        );
        for owner in owners {
            assert_eq!(owner["owner_completed"], true, "direct bridge listener owner survived");
        }
        // The already-collected CLI owns auth/resource/Live; host has its own pidfd.
        self.1.assert_closed();
    }

    // M5 cancellation owns the delivered child directly; its adapter migration
    // remains separate from the normal runtime child collection below.
    pub fn raw_command(&self, bridge: &str) -> Command {
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
        let pinned = include_str!("../../../../hassmic.pin")
            .lines()
            .find_map(|line| line.strip_prefix("commit="))
            .unwrap();
        assert_eq!(value["hassmic_commit"], pinned);
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
        for endpoint in &endpoints {
            assert_eq!(endpoint.ip(), std::net::Ipv4Addr::LOCALHOST);
        }
        self.owners_closed();
        endpoints
    }

    pub fn owners_closed(&self) {
        // The CLI's collected exit owns the in-process spoof/Live tasks. Its
        // separate bridge child and Linux host require their own receipts.
        cli_owner::assert_closed(&self.0);
        self.1.assert_closed();
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
