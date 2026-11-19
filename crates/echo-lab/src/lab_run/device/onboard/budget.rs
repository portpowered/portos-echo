//! Without a relay in the Wyoming path, budgets are measured from the bridge's own trace.
use crate::lab_run::{
    device::{Configured, observations, runtime::wire},
    run_options::Options,
    run_report::Artifacts,
};
use serde_json::{Value, json};

// These journeys are judged on host wire receipts that no on-device bridge passes through.
const WIRED: [&str; 4] = [
    "m3-full-duplex",
    "session-two-turns",
    "session-idle-timeout",
    "session-barge-continues",
];

pub(in crate::lab_run::device) fn admit(options: &Options) -> Result<(), String> {
    for path in &options.scenarios {
        let scenario = crate::lab_validate::load(path)?;
        if let Some(id) = scenario["id"].as_str().filter(|id| WIRED.contains(id)) {
            return Err(format!(
                "device: unmet prerequisite: {id} needs the host wire observer; a bridge on the Echo has no relay in its Wyoming path"
            ));
        }
    }
    Ok(())
}

/// Same ceiling, percentile rule and verdict text as the host wire measurement.
pub(super) fn check(records: &[Value], run: Option<&str>, ceiling: f64) -> Result<Value, String> {
    if !ceiling.is_finite() || ceiling < 0.0 {
        return Err("device: invalid M4 latency ceiling".into());
    }
    let run = run.ok_or("device: unmet prerequisite: completed M4 run in the bridge trace from the Echo")?;
    let mut samples = records
        .iter()
        .filter(|record| {
            record["event"] == "timing" && record["name"] == "first_audio_after_endpoint_ms" && record["run_id"] == run
        })
        .map(|record| {
            record["duration_ms"]
                .as_f64()
                .filter(|sample| sample.is_finite() && *sample >= 0.0)
                .ok_or("device: invalid M4 timing sample in the bridge trace from the Echo")
        })
        .collect::<Result<Vec<_>, _>>()?;
    if samples.is_empty() {
        return Err(
            "device: unmet prerequisite: first_audio_after_endpoint_ms timing sample in the bridge trace from the Echo"
                .into(),
        );
    }
    samples.sort_by(f64::total_cmp);
    let measured = samples[samples.len() - samples.len() / 20 - 1];
    if measured > ceiling {
        return Err(format!(
            "device: first_audio_after_endpoint_ms_p95 observed {measured} ms exceeds {ceiling} ms"
        ));
    }
    Ok(json!({"event":"device.budget","scenario":"m4-spoof-tool","run_id":run,
        "name":"first_audio_after_endpoint_ms_p95","measured":measured,"limit":ceiling,
        "unit":"ms","rule":"<=","source":"bridge trace timing records read from the Echo",
        "samples":samples.len(),"passes":true}))
}

impl Configured<'_> {
    pub(in crate::lab_run::device) fn trace_budget(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
        if self.options.on_device.is_none() || !self.state.started {
            return Ok(());
        }
        let Some(ceiling) = wire::ceiling(self.options)? else {
            return Ok(());
        };
        self.state.observe()?;
        let measured = check(&self.state.trace.records, self.state.run.as_deref(), ceiling);
        artifacts.checks.insert("device_m4_latency".into(), measured.is_ok());
        observations::append(artifacts, &measured?)
    }
}
