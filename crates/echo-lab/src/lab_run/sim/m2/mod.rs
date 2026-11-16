//! M2 owns a fresh production world per credential outcome.
pub mod auth;
mod journey;
mod oracle;
#[cfg(test)]
mod tests;

use crate::lab_run::{Driver, Step, run_options::Options, run_report::Artifacts};
use serde_json::{Value, json};
use std::time::Duration;

pub fn negative(scenario: &Value) -> Result<Vec<Step>, String> {
    if scenario["id"] != "m2-live-connect" {
        return Ok(vec![]);
    }
    let cases = scenario["negative"].as_array().ok_or("m2: missing negative cases")?;
    if cases.len() != 2 {
        return Err("m2: expected invalid and expired cases".into());
    }
    let mut steps = vec![];
    for (case, expected) in cases.iter().zip(["invalid", "expired"]) {
        if case["inject"] != json!({"credential":expected}) {
            return Err("m2: unknown credential injection".into());
        }
        steps.push(Step::Inject);
        for (name, value) in case.as_object().ok_or("m2: invalid negative case")? {
            match name.as_str() {
                "inject" => {}
                "expect_trace_absent" => steps.push(Step::TraceAbsent(
                    value.as_array().ok_or("m2: invalid absent trace")?.clone(),
                )),
                _ => steps.push(super::parse(&json!({name:value}))?),
            }
        }
    }
    Ok(steps)
}

pub struct M2<'a> {
    options: &'a Options,
    runtime: tokio::runtime::Runtime,
    case: usize,
    measured: Option<Value>,
    utterance: Option<std::path::PathBuf>,
}

impl<'a> M2<'a> {
    pub fn new(options: &'a Options) -> Result<Self, String> {
        Ok(Self {
            options,
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|_| "m2: runtime unavailable")?,
            case: 0,
            measured: None,
            utterance: None,
        })
    }
}

impl Driver for M2<'_> {
    fn execute(
        &mut self,
        step: &Step,
        _index: usize,
        _next: Option<&Step>,
        artifacts: &mut Artifacts,
    ) -> Result<(), String> {
        match step {
            Step::SayFile(_) | Step::Inject => {
                if let Step::SayFile(path) = step {
                    self.utterance = Some(path.clone());
                }
                let case = ["success", "invalid", "expired"]
                    .get(self.case)
                    .ok_or("m2: extra case")?;
                let (measured, result) = self.runtime.block_on(journey::run(
                    self.options,
                    &artifacts.root,
                    case,
                    self.utterance.as_deref().ok_or("m2: missing utterance")?,
                ));
                let file = format!("m2-{case}.json");
                std::fs::write(artifacts.root.join(&file), measured.to_string())
                    .map_err(|_| "m2: cannot publish evidence")?;
                artifacts.evidence.push(file);
                self.measured = Some(measured);
                self.case += 1;
                result
            }
            Step::Ui(expected) => oracle::screen(self.measured.as_ref().ok_or("m2: missing measurement")?, expected),
            Step::Trace(values) => {
                oracle::trace(self.measured.as_ref().ok_or("m2: missing measurement")?, values, false)
            }
            Step::TraceAbsent(values) => {
                oracle::trace(self.measured.as_ref().ok_or("m2: missing measurement")?, values, true)
            }
            _ => Err("m2: unsupported step".into()),
        }
    }

    fn close(&mut self, _artifacts: &mut Artifacts) -> Result<(), String> {
        if self.case != 3 {
            return Err("m2: incomplete credential cases".into());
        }
        Ok(())
    }
}

const DEADLINE: Duration = Duration::from_secs(30);
