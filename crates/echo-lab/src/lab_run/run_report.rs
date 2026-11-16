use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::{Value, json};

pub use echo_lab::lab_mcp::LinkCounts;

pub struct Artifacts {
    pub sim: bool,
    pub m1: Option<Value>,
    pub root: PathBuf,
    pub checks: BTreeMap<String, bool>,
    pub blocked: BTreeMap<String, String>,
    pub sessions: Vec<String>,
    pub evidence: Vec<String>,
    pub link_counts: Option<LinkCounts>,
    link_expectations: Vec<(usize, LinkCounts)>,
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
            link_counts: None,
            link_expectations: Vec::new(),
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

    pub fn expect_link_counts(&mut self, step: usize, expected: &LinkCounts) -> Result<(), String> {
        self.compare_link_counts(expected)?;
        self.link_expectations.push((step, *expected));
        Ok(())
    }

    fn compare_link_counts(&self, expected: &LinkCounts) -> Result<(), String> {
        let actual = self.link_counts.ok_or("link counts: measurement unavailable")?;
        if actual != *expected {
            let expected = serde_json::to_string(expected).map_err(|_| "link counts: cannot encode expected")?;
            let actual = serde_json::to_string(&actual).map_err(|_| "link counts: cannot encode actual")?;
            return Err(format!("link counts: expected {expected}; actual {actual}"));
        }
        Ok(())
    }

    pub fn finalize_link_counts(&mut self) -> Result<(), String> {
        let mut failures = Vec::new();
        for (step, expected) in &self.link_expectations {
            if let Err(error) = self.compare_link_counts(expected) {
                self.checks.insert(format!("step_{step}"), false);
                failures.push(format!("step {step}: {error}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
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
        if let Some(counts) = self.link_counts {
            report["link_counts"] = serde_json::to_value(counts).map_err(|_| "run report: cannot encode counts")?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lab_run::{Driver, Step, dispatch, run_oracles};

    fn counts() -> Value {
        json!({"link":1,"tools_list":1,"tool_call":1,"forced_refresh":2,
            "replay_rejected":1,"relink_error":1,"restart_grant_kept":1,"unlink":1})
    }

    fn typed(value: Value) -> LinkCounts {
        serde_json::from_value(value).unwrap()
    }

    struct Observed(LinkCounts);

    impl Driver for Observed {
        fn execute(&mut self, _: &Step, _: usize, _: Option<&Step>, artifacts: &mut Artifacts) -> Result<(), String> {
            artifacts.link_counts = Some(self.0);
            Ok(())
        }

        fn close(&mut self, _: &mut Artifacts) -> Result<(), String> {
            Ok(())
        }
    }

    fn steps() -> Vec<Step> {
        vec![
            Step::Unsupported,
            run_oracles::parse(&json!({"expect_link_counts":counts()})).unwrap(),
        ]
    }

    fn report(artifacts: &mut Artifacts, result: &Result<(), String>) -> Value {
        artifacts
            .publish(
                "M5",
                "T3",
                "synthetic-digest",
                result.as_ref().err().map(String::as_str),
                false,
            )
            .unwrap();
        serde_json::from_slice(&fs::read(artifacts.root.join("report.json")).unwrap()).unwrap()
    }

    #[test]
    fn link_count_oracle_reports_every_observed_counter_mismatch() {
        let root = tempfile::tempdir().unwrap();
        let mut artifacts = Artifacts::create(root.path().join("run")).unwrap();
        artifacts.sim = true;
        let expected = counts();
        for field in expected.as_object().unwrap().keys() {
            let mut actual = expected.clone();
            actual[field] = json!(actual[field].as_u64().unwrap() + 1);
            let result = dispatch(&mut Observed(typed(actual.clone())), &steps(), &mut artifacts);
            let message = result.as_ref().unwrap_err();
            assert!(message.starts_with("step 1: link counts: expected {"));
            let (want, got) = message
                .strip_prefix("step 1: link counts: expected ")
                .unwrap()
                .split_once("; actual ")
                .unwrap();
            assert_eq!(serde_json::from_str::<Value>(want).unwrap(), expected);
            assert_eq!(serde_json::from_str::<Value>(got).unwrap(), actual);
            let report = report(&mut artifacts, &result);
            assert_eq!(report["result"], "fail");
            assert_eq!(report["link_counts"], actual);
            assert_eq!(report["checks"], json!({"step_0":true,"step_1":false}));
            assert!(!artifacts.root.join("report.pending").exists());
        }
    }

    #[test]
    fn link_count_oracle_never_infers_measurements_from_expectations() {
        let root = tempfile::tempdir().unwrap();
        let mut artifacts = Artifacts::create(root.path().join("run")).unwrap();
        artifacts.sim = true;
        let expected = Step::LinkCounts(LinkCounts::default());
        let result = dispatch(&mut Observed(LinkCounts::default()), &[expected], &mut artifacts);
        assert_eq!(result, Err("step 0: link counts: measurement unavailable".into()));
        let report = report(&mut artifacts, &result);
        assert_eq!(report["result"], "fail");
        assert_eq!(report["checks"], json!({"step_0":false}));
        assert!(report.get("link_counts").is_none());
        let events = fs::read_to_string(artifacts.root.join("events.jsonl")).unwrap();
        let last: Value = serde_json::from_str(events.lines().last().unwrap()).unwrap();
        assert_eq!(last["event"], "step_failed");
        assert_eq!(last["message"], "link counts: measurement unavailable");
        assert_eq!(last["step"], 0);
    }

    #[test]
    fn link_count_oracle_publishes_exact_counts_and_preserves_cleanup_failures() {
        let root = tempfile::tempdir().unwrap();
        let mut artifacts = Artifacts::create(root.path().join("run")).unwrap();
        artifacts.sim = true;
        let result = dispatch(&mut Observed(typed(counts())), &steps(), &mut artifacts);
        assert_eq!(result, Ok(()));
        let passed = report(&mut artifacts, &result);
        assert_eq!(passed["result"], "pass");
        assert_eq!(passed["link_counts"], counts());
        assert_eq!(passed["checks"], json!({"step_0":true,"step_1":true}));
        let failed = report(&mut artifacts, &Err("cleanup: observer failed".into()));
        assert_eq!(failed["result"], "fail");
        assert_eq!(failed["link_counts"], counts());
        assert_eq!(failed["limitations"][0], "cleanup: observer failed");
    }

    #[test]
    fn link_count_oracle_rechecks_late_observations_before_publication() {
        let root = tempfile::tempdir().unwrap();
        let mut artifacts = Artifacts::create(root.path().join("run")).unwrap();
        artifacts.sim = true;
        assert_eq!(
            dispatch(&mut Observed(typed(counts())), &steps(), &mut artifacts),
            Ok(())
        );
        assert_eq!(artifacts.finalize_link_counts(), Ok(()));
        artifacts.link_counts.as_mut().unwrap().tool_call += 1;
        let result = artifacts.finalize_link_counts();
        assert!(
            result
                .as_ref()
                .unwrap_err()
                .starts_with("step 1: link counts: expected {")
        );
        let failed = report(&mut artifacts, &result);
        assert_eq!(failed["result"], "fail");
        assert_eq!(failed["checks"], json!({"step_0":true,"step_1":false}));
        assert_eq!(failed["link_counts"]["tool_call"], 2);
    }
}
