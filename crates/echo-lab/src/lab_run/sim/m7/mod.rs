//! Authored M7 success/deadline scenario through prebuilt production processes.
mod driver;
mod live;
mod muse;
mod oracle;
mod world;
use crate::lab_run::Step;
pub use driver::M7;
use serde_json::{Value, json};

pub fn utterance(value: &str) -> Result<Step, String> {
    if value != "Computer, ask Muse to run the test job" {
        return Err("m7: no finite fixture for utterance".into());
    }
    Ok(Step::SayText(value.into()))
}

pub fn negative(scenario: &Value) -> Result<Vec<Step>, String> {
    if scenario["id"] != "m7-muse" {
        return Ok(vec![]);
    }
    if scenario["setup"] != json!({"mcp":"spoof","backend":"muse"}) {
        return Err("m7: expected explicit Muse/spoof setup".into());
    }
    let cases = scenario["negative"].as_array().ok_or("m7: missing negative cases")?;
    if cases.len() != 1 || cases[0]["inject"] != json!({"muse":"no_report"}) {
        return Err("m7: expected one explicit no_report case".into());
    }
    let mut steps = vec![Step::Inject];
    for (name, value) in cases[0].as_object().ok_or("m7: invalid negative case")? {
        match name.as_str() {
            "inject" => {}
            "expect_trace_absent" => {
                let values = value.as_array().ok_or("m7: invalid absent selectors")?;
                super::selectors::parse(values)?;
                steps.push(Step::TraceAbsent(values.clone()));
            }
            _ => steps.push(super::parse(&json!({name:value}))?),
        }
    }
    Ok(steps)
}
