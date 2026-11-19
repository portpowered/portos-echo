//! Scenario voice settings are config overlays, never measured-budget passes.
use super::super::super::{run_options::Options, run_report::Artifacts};
use serde_json::{Map, Value, json};

pub fn is_setting(name: &str) -> bool {
    matches!(name, "session_listen_seconds" | "session_max_seconds")
}

pub fn settings(scenario: &Value) -> Result<Map<String, Value>, String> {
    let mut result = Map::new();
    for (name, value) in scenario["budgets"].as_object().ok_or("device: missing budgets")? {
        if is_setting(name) {
            let number = value
                .as_u64()
                .filter(|number| *number <= u64::from(u32::MAX))
                .ok_or_else(|| format!("device: invalid voice budget {name}: expected integer seconds"))?;
            result.insert(name.clone(), number.into());
        }
    }
    Ok(result)
}

pub fn collect(options: &Options) -> Result<Map<String, Value>, String> {
    let mut result = Map::new();
    for path in &options.scenarios {
        for (name, value) in settings(&crate::lab_validate::load(path)?)? {
            if result.get(&name).is_some_and(|previous| previous != &value) {
                return Err(format!("device: conflicting voice budget {name} in scenario batch"));
            }
            result.insert(name, value);
        }
    }
    Ok(result)
}

pub fn validate(original: &str, derived: &str, settings: &Map<String, Value>) -> Result<(), String> {
    let mut expected =
        echo_config::BridgeConfig::from_toml_str(original).map_err(|_| "device: invalid original configuration")?;
    let actual = echo_config::BridgeConfig::from_toml_str(derived)
        .map_err(|error| format!("device: invalid voice budget: {error}"))?;
    for (name, value) in settings {
        let seconds = value
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or("device: invalid seconds")?;
        match name.as_str() {
            "session_listen_seconds" => expected.voice.session_listen_seconds = seconds,
            "session_max_seconds" => expected.voice.session_max_seconds = seconds,
            _ => return Err(format!("device: unsupported voice setting {name}")),
        }
    }
    if expected != actual {
        return Err("device: voice overlay changed other bridge settings".into());
    }
    Ok(())
}

pub fn record(options: &Options, config: &str, artifacts: &mut Artifacts) -> Result<(), String> {
    let settings = collect(options)?;
    if settings.is_empty() {
        return Ok(());
    }
    let effective = echo_config::BridgeConfig::from_toml_str(config)
        .map_err(|_| "device: invalid effective configuration")?
        .voice;
    super::super::observations::append(
        artifacts,
        &json!({
            "event":"device.effective_voice", "step":null,
            "session_listen_seconds":effective.session_listen_seconds,
            "session_max_seconds":effective.session_max_seconds,
            "scenario_settings":settings
        }),
    )
}
