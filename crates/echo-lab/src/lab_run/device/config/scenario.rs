//! Admission and honest evidence disposition for configured device journeys.
use super::super::super::{run_options::Options, run_report::Artifacts};
use serde_json::{Value, json};

pub fn admits(scenario: &Value) -> Result<bool, String> {
    if scenario["id"] != "m2-live-connect" {
        return Ok(false);
    }
    if !scenario["tiers"]
        .as_array()
        .is_some_and(|tiers| tiers.iter().any(|tier| tier == "T5"))
    {
        return Err("device: scenario does not admit T5".into());
    }
    if let Some(name) = scenario["setup"]
        .as_object()
        .ok_or("device: missing setup")?
        .keys()
        .next()
    {
        return Err(format!("device: unsupported setup {name}"));
    }
    if let Some(name) = scenario["budgets"]
        .as_object()
        .ok_or("device: missing budgets")?
        .keys()
        .next()
    {
        return Err(format!("device: unsupported budget {name}"));
    }
    Ok(true)
}

pub fn record(options: &Options, artifacts: &mut Artifacts) -> Result<(), String> {
    if options.sim || options.bridge_config.is_none() {
        return Ok(());
    }
    for path in &options.scenarios {
        let scenario = crate::lab_validate::load(path)?;
        if scenario["id"] != "m2-live-connect" && !options.live_real() {
            continue;
        }
        if !options.live_real() {
            artifacts.device_limits.push("live: fake (not milestone proof)".into());
        }
        super::super::observations::append(
            artifacts,
            &json!({
                "event":"device.startup_mode", "step":null,
                "live":if options.live_real() { "real" } else { "fake" },
                "limitation":if options.live_real() { "physical proof requires coordinator T5" }
                    else { "live: fake (not milestone proof)" }
            }),
        )?;
        for (index, _) in scenario["negative"]
            .as_array()
            .ok_or("device: missing negatives")?
            .iter()
            .enumerate()
        {
            artifacts.device_limits.push(format!(
                "{} negative {index}: not run at T5 (T3/T4 cover it)",
                scenario["id"].as_str().ok_or("device: missing scenario ID")?
            ));
            super::super::observations::append(
                artifacts,
                &json!({
                    "event":"device.negative", "step":null, "scenario":scenario["id"],
                    "negative_index":index, "disposition":"not run at T5 (T3/T4 cover it)"
                }),
            )?;
        }
    }
    Ok(())
}
