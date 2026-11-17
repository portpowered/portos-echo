//! Finalize each original process trace before reusing its output path.
use super::super::run_report::Artifacts;
use super::{observations, runtime::Runtime};
use serde_json::json;

impl Runtime {
    pub(super) fn collect(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
        let result = self.observe().and_then(|()| self.trace.finish());
        self.invalid |= result.is_err();
        if result.is_err() {
            artifacts.checks.remove("bridge_errors_absent");
        }
        let mut failures = Vec::new();
        for (index, selectors, run, offset) in &self.expectations {
            if let Err(error) = result.as_ref().map_err(Clone::clone).and_then(|()| {
                super::super::sim::selectors::check(&self.trace.records[*offset..], selectors, run.as_deref())
            }) {
                artifacts.checks.insert(format!("step_{index}"), false);
                failures.push(error);
            }
        }
        if result.is_ok() {
            for record in &self.trace.records {
                failures.extend(artifacts.observation(record).err());
                failures.extend(observations::mute_window(artifacts, record).err());
                if record["event"] == "mic.mute_window" {
                    self.window = Some(json!({"received_frames":record["received_frames"],
                        "forwarded_frames":record["forwarded_frames"]}));
                }
            }
            self.errors += self
                .trace
                .records
                .iter()
                .filter(|r| r["event"] == "bridge.error")
                .count();
            artifacts.checks.insert("bridge_errors_absent".into(), self.errors == 0);
            if self.errors != 0 {
                failures.push("device: bridge errors observed".into());
            }
        }
        failures.extend(result.err());
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    pub(super) fn summary(&self, artifacts: &mut Artifacts) -> Result<(), String> {
        if self.invalid || !self.started || self.generation == 0 {
            return Ok(());
        }
        artifacts.m1 = Some(json!({"ready_before_first_wake":self.ready,
        "streamed_wakes":self.wakes,"bridge_restarts":self.bridge_restarts,
        "hassmic_host_restarts":self.host_restarts,
        "mute_status":match &self.window {
            None => "blocked",
            Some(window) if window == &json!({"received_frames":0,"forwarded_frames":0}) => "pass",
            Some(_) => "fail"
        }}));
        if let Some(window) = &self.window {
            artifacts.m1.as_mut().ok_or("device: missing summary")?["mute_window"] = window.clone();
        }
        if let Some(clip) = &self.clip {
            let summary = artifacts.m1.as_mut().ok_or("device: missing summary")?;
            summary["wake_clip"] = clip.clone();
            summary["plays"] = json!(self.plays);
        }
        observations::append(
            artifacts,
            &json!({"event":"device.summary","step":null,
            "elapsed_ms":self.elapsed()?,"bridge_errors":self.errors,
            "bridge_restarts_recovered":self.bridge_restarts,
            "hassmic_restarts_recovered":self.host_restarts}),
        )
    }

    pub(super) fn elapsed(&self) -> Result<u128, String> {
        Ok(self.began.ok_or("device: missing start time")?.elapsed().as_millis())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_no_clip_summary_preserves_exact_serialization() {
        let root = tempfile::tempdir().unwrap();
        let mut artifacts = Artifacts::create(root.path().join("out")).unwrap();
        let mut runtime = Runtime::default();
        runtime.started = true;
        runtime.generation = 1;
        runtime.ready = 1;
        runtime.began = Some(std::time::Instant::now());
        runtime.summary(&mut artifacts).unwrap();
        let encoded = serde_json::to_string(artifacts.m1.as_ref().unwrap()).unwrap();
        assert_eq!(
            encoded,
            r#"{"bridge_restarts":0,"hassmic_host_restarts":0,"mute_status":"blocked","ready_before_first_wake":1,"streamed_wakes":[]}"#
        );
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../../../schemas/report.schema.json")).unwrap();
        assert!(
            jsonschema::validator_for(&schema["properties"]["m1"])
                .unwrap()
                .is_valid(artifacts.m1.as_ref().unwrap())
        );
    }
    #[test]
    fn device_invalid_later_generation_revokes_prior_zero_error_claim() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("trace");
        std::fs::write(&path, "").unwrap();
        let mut runtime = Runtime::default();
        runtime.trace_path = path.clone();
        let mut artifacts = Artifacts::create(root.path().join("out")).unwrap();
        runtime.collect(&mut artifacts).unwrap();
        assert_eq!(artifacts.checks.get("bridge_errors_absent"), Some(&true));
        std::fs::write(&path, "{\"event\":").unwrap();
        assert!(runtime.collect(&mut artifacts).is_err());
        assert!(!artifacts.checks.contains_key("bridge_errors_absent"));
        runtime.summary(&mut artifacts).unwrap();
        assert!(artifacts.m1.is_none());
    }
}
