use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
pub const FAKE: &str = env!("CARGO_BIN_EXE_echo-lab-fake-run");

pub struct Session(pub PathBuf);

impl Session {
    pub fn new(fixture: &Value) -> Self {
        let root = std::env::temp_dir().join(format!(
            "lab run test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("fixture.json"), fixture.to_string()).unwrap();
        let session = Self(root.canonicalize().unwrap());
        session.scenario("one.yaml", &json!([{"say":"utterance.wav"}]));
        fs::write(session.0.join("utterance.wav"), b"synthetic input fixture").unwrap();
        session
    }

    pub fn scenario(&self, name: &str, steps: &Value) {
        fs::write(
            self.0.join(name),
            json!({"id":"fixture", "milestone":"B0", "tiers":["T3"],
        "setup":{}, "steps":steps, "budgets":{}, "negative":[]})
            .to_string(),
        )
        .unwrap();
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_echo-lab"));
        command
            .current_dir(&self.0)
            .env("ECHO_LAB_RING", "fixture-ring")
            .env("ECHO_LAB_RUN_FIXTURE", self.0.join("fixture.json"))
            .env("ECHO_LAB_RUN_LEDGER", self.0.join("ledger.jsonl"));
        command
    }

    pub fn run_command(&self, extra: &[&str]) -> Command {
        let mut command = self.command();
        command
            .args([
                "run",
                "one.yaml",
                "--driver",
                "device",
                "--bench",
                "ring",
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
            ])
            .args(extra);
        command
    }

    pub fn run(&self, extra: &[&str]) -> Output {
        self.run_command(extra).output().unwrap()
    }

    pub fn ledger(&self) -> Vec<Value> {
        fs::read_to_string(self.0.join("ledger.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }

    pub fn operations(&self) -> Vec<String> {
        self.ledger()
            .iter()
            .map(|v| v["operation"].as_str().unwrap().to_owned())
            .collect()
    }

    pub fn report(&self) -> Value {
        let report: Value =
            serde_json::from_slice(&fs::read(self.0.join("artifacts").join("report.json")).unwrap()).unwrap();
        let schema: Value = serde_json::from_str(include_str!("../../../../schemas/report.schema.json")).unwrap();
        assert!(jsonschema::validator_for(&schema).unwrap().is_valid(&report));
        report
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn failed(output: &Output, diagnostic: &str) {
    assert_eq!(output.status.code(), Some(1));
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(errors.contains(diagnostic), "{errors}");
    assert!(!errors.contains("fixture-private-child-output"));
}
