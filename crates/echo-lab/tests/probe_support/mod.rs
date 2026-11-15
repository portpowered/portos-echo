use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
pub const FAKE: &str = env!("CARGO_BIN_EXE_echo-lab-fake-adb");
pub const SERIALS: [&str; 2] = ["FAKESERIAL123", "FAKESERIAL456"];

pub struct Session(pub PathBuf);

impl Session {
    pub fn new(overrides: &Value) -> Self {
        let path = std::env::temp_dir().join(format!(
            "lab-probe-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::write(path.join("overrides.json"), serde_json::to_vec(overrides).unwrap()).unwrap();
        Self(path)
    }

    pub fn command(&self, destination: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_echo-lab"));
        command
            .arg("probe")
            .arg("--adb")
            .arg(FAKE)
            .arg("--out")
            .arg(destination)
            .env("ECHO_LAB_FAKE_FIXTURE", self.0.join("overrides.json"))
            .env("ECHO_LAB_FAKE_LEDGER", self.0.join("ledger.jsonl"))
            .current_dir(&self.0);
        command
    }

    pub fn run(&self) -> Output {
        self.command(&self.0.join("out")).output().unwrap()
    }

    pub fn ledger(&self) -> Vec<Value> {
        fs::read_to_string(self.0.join("ledger.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn response(text: &str) -> Value {
    json!({"stdout": text, "stderr": ""})
}

pub fn clean(bytes: &[u8]) {
    let text = String::from_utf8_lossy(bytes);
    for serial in SERIALS {
        assert!(!text.contains(serial), "unscrubbed synthetic identifier");
    }
}

pub fn scan(path: &Path) -> usize {
    let mut count = 0;
    for entry in fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            count += scan(&entry.path());
        } else {
            clean(&fs::read(entry.path()).unwrap());
            count += 1;
        }
    }
    count
}

pub fn manifest(session: &Session, output: &Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stderr, b"");
    let persisted = fs::read(session.0.join("out/manifest.json")).unwrap();
    assert_eq!(output.stdout, persisted);
    let value: Value = serde_json::from_slice(&persisted).unwrap();
    let schema: Value = serde_json::from_str(include_str!("../../data/probe.schema.json")).unwrap();
    let validator = jsonschema::options()
        .should_validate_formats(true)
        .build(&schema)
        .unwrap();
    assert!(
        validator.is_valid(&value),
        "{:?}",
        validator.iter_errors(&value).map(|e| e.to_string()).collect::<Vec<_>>()
    );
    value
}
