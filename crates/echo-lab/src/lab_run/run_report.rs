use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::{Value, json};

pub struct Artifacts {
    pub sim: bool,
    pub m1: Option<Value>,
    pub root: PathBuf,
    pub checks: BTreeMap<String, bool>,
    pub blocked: BTreeMap<String, String>,
    pub sessions: Vec<String>,
    pub evidence: Vec<String>,
    observations: Vec<Value>,
    events: File,
    started: Instant,
}

impl Artifacts {
    pub fn create(root: PathBuf) -> Result<Self, String> {
        fs::create_dir(&root).map_err(|_| "run output: cannot create fresh directory")?;
        let events = File::create(root.join("events.jsonl")).map_err(|_| "run output: cannot create events")?;
        Ok(Self {
            sim: false,
            m1: None,
            observations: Vec::new(),
            root,
            checks: BTreeMap::new(),
            blocked: BTreeMap::new(),
            sessions: Vec::new(),
            evidence: vec!["events.jsonl".into()],
            events,
            started: Instant::now(),
        })
    }

    pub fn event(&mut self, event: &str, step: Option<usize>, message: Option<&str>) -> Result<(), String> {
        let value = json!({"event":event, "scenario":"batch", "step":step,
            "elapsed_ms":self.started.elapsed().as_millis(), "message":message});
        writeln!(self.events, "{value}").map_err(|_| "run output: cannot append event".into())
    }

    pub fn observation(&mut self, record: &Value) -> Result<(), String> {
        if !matches!(
            record["event"].as_str(),
            Some("wyoming.run_started" | "wyoming.mic_frames" | "wyoming.run_ended")
        ) {
            return Ok(());
        }
        let mut safe = json!({"event":record["event"],"run_id":record["run_id"],"elapsed_ms":record["elapsed_ms"]});
        if record["event"] == "wyoming.mic_frames" {
            safe["count"] = record["count"].clone();
            self.observations.push(json!({"event":"wyoming.mic_frames",
                "run_id":record["run_id"], "count":record["count"]}));
        }
        writeln!(self.events, "{safe}").map_err(|_| "run output: cannot append observation".into())
    }

    pub fn endpoint(&mut self, address: std::net::SocketAddr) -> Result<(), String> {
        let safe = json!({"event":"sim.endpoint","address":address.to_string()});
        writeln!(self.events, "{safe}").map_err(|_| "run output: cannot append endpoint".into())
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
        let mut report = json!({
            "milestone":milestone, "result":self.outcome(error),
            "placement":"bridge_pc", "build_commit":build_commit(), "device":null,
            "evidence":self.evidence, "checks":self.checks, "observations":self.observations,
            "limitations":error.into_iter().chain(self.blocked.values().map(String::as_str)).chain(if self.sim { vec!["pinned hassmic-host binds wildcard listeners (upstream; no bind-address option)", "T3 simulation does not prove a physical milestone or real-provider compatibility."] } else { vec!["Device edges require independent operator verification; no physical milestone is proved."] }).collect::<Vec<_>>(),
            "tier":tier, "driver":if self.sim { "sim" } else { "device" }, "scenario_digest":digest,
            "hassmic_commit":if self.sim { include_str!("../../../../hassmic.pin").lines().find_map(|l| l.strip_prefix("commit=")) } else { None }, "bench_session_ids":self.sessions
        });
        if let Some(m1) = &self.m1 {
            report["m1"] = m1.clone();
        }
        validate(&report)?;
        let text = serde_json::to_vec_pretty(&report).map_err(|_| "run output: cannot encode report")?;
        fs::write(self.root.join("report.pending"), text).map_err(|_| "run output: cannot write report")?;
        fs::rename(self.root.join("report.pending"), self.root.join("report.json"))
            .map_err(|_| "run output: cannot publish report".into())
    }

    pub fn outcome(&self, error: Option<&str>) -> &'static str {
        if error.is_some() {
            "fail"
        } else if self.blocked.is_empty() {
            "pass"
        } else {
            "blocked"
        }
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
