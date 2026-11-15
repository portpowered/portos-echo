use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::{Value, json};

pub struct Artifacts {
    pub root: PathBuf,
    pub checks: BTreeMap<String, bool>,
    pub sessions: Vec<String>,
    events: File,
    started: Instant,
}

impl Artifacts {
    pub fn create(root: PathBuf) -> Result<Self, String> {
        fs::create_dir(&root).map_err(|_| "run output: cannot create fresh directory")?;
        let events = File::create(root.join("events.jsonl")).map_err(|_| "run output: cannot create events")?;
        Ok(Self {
            root,
            checks: BTreeMap::new(),
            sessions: Vec::new(),
            events,
            started: Instant::now(),
        })
    }

    pub fn event(&mut self, event: &str, step: Option<usize>, message: Option<&str>) -> Result<(), String> {
        let value = json!({"event":event, "scenario":"batch", "step":step,
            "elapsed_ms":self.started.elapsed().as_millis(), "message":message});
        writeln!(self.events, "{value}").map_err(|_| "run output: cannot append event".into())
    }

    pub fn publish(
        &mut self,
        milestone: &str,
        tier: &str,
        digest: &str,
        error: Option<&str>,
        keep: bool,
    ) -> Result<(), String> {
        if error.is_none() && !keep {
            for entry in fs::read_dir(&self.root).map_err(|_| "cleanup: cannot list recordings")? {
                let path = entry.map_err(|_| "cleanup: cannot read recording entry")?.path();
                if path.extension().is_some_and(|ext| ext == "wav") {
                    fs::remove_file(path).map_err(|_| "cleanup: cannot delete passing audio")?;
                }
            }
        }
        self.events.sync_all().map_err(|_| "run output: cannot flush events")?;
        let report = json!({
            "milestone":milestone, "result":if error.is_none() { "pass" } else { "fail" },
            "placement":"bridge_pc", "build_commit":build_commit(), "device":null,
            "evidence":["events.jsonl"], "checks":self.checks,
            "limitations":error.into_iter().chain(["Device edges require independent operator verification; no physical milestone is proved."]).collect::<Vec<_>>(),
            "tier":tier, "driver":"device", "scenario_digest":digest,
            "hassmic_commit":null, "bench_session_ids":self.sessions
        });
        validate(&report)?;
        let text = serde_json::to_vec_pretty(&report).map_err(|_| "run output: cannot encode report")?;
        fs::write(self.root.join("report.pending"), text).map_err(|_| "run output: cannot write report")?;
        fs::rename(self.root.join("report.pending"), self.root.join("report.json"))
            .map_err(|_| "run output: cannot publish report".into())
    }
}

fn build_commit() -> String {
    let result = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output();
    result
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_else(|| "unknown".into())
}

fn validate(report: &Value) -> Result<(), String> {
    let schema = serde_json::from_str(include_str!("../../../../schemas/report.schema.json"))
        .map_err(|_| "run report: invalid embedded JSON")?;
    let validator = jsonschema::validator_for(&schema).map_err(|_| "run report: invalid embedded schema")?;
    if !validator.is_valid(report) {
        return Err("run report: schema validation failed".into());
    }
    Ok(())
}
