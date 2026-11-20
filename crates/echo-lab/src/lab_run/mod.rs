mod device;
mod run_command;
mod run_device;
mod run_jpeg;
mod run_lease;
mod run_options;
mod run_oracles;
mod run_report;
mod sim;

use std::io::Write;
use std::path::{Path, PathBuf};

use run_options::Options;
use run_report::Artifacts;
pub use run_report::LinkCounts;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpAction {
    Link,
    Approve,
    Refresh,
    Restart,
    Call,
    Replay,
    Probe,
    Unlink,
}

impl McpAction {
    fn name(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Approve => "approve",
            Self::Refresh => "refresh",
            Self::Restart => "restart",
            Self::Call => "call",
            Self::Replay => "replay",
            Self::Probe => "probe",
            Self::Unlink => "unlink",
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct McpControl {
    action: McpAction,
}

/// One step representation and dispatcher, shared by future drivers.
pub enum Step {
    SayFile(PathBuf),
    SayText(String),
    Speech(Vec<String>),
    Screen(PathBuf),
    Frame(String),
    Trace(Vec<serde_json::Value>),
    Ui(serde_json::Value),
    Press(String),
    Budget(String, f64),
    Restart { process: String, count: u64, say: PathBuf },
    LinkCounts(LinkCounts),
    Mcp(McpAction),
    Inject,
    TraceAbsent(Vec<serde_json::Value>),
    Unsupported,
}

pub trait Driver {
    fn execute(
        &mut self,
        step: &Step,
        index: usize,
        next: Option<&Step>,
        artifacts: &mut Artifacts,
    ) -> Result<(), String>;
    fn close(&mut self, artifacts: &mut Artifacts) -> Result<(), String>;
}

fn say(value: &str, scenario: &str, options: &Options) -> Result<Step, String> {
    device::pcm::persistent_input(value, scenario, options)?.map_or_else(|| ordinary_say(value, scenario, options), Ok)
}

fn ordinary_say(value: &str, scenario: &str, options: &Options) -> Result<Step, String> {
    if options.wake_clip.is_some() {
        return Ok(Step::SayFile(PathBuf::new()));
    }
    if options.sim {
        match crate::lab_validate::load(scenario)?["id"].as_str() {
            Some("m3-full-duplex" | "session-barge-continues") => return sim::m3::utterance(value),
            Some(id) if sim::session::goodbye::admits(id) => return sim::session::goodbye::utterance(id, value),
            Some(id) if sim::session::time_answers::admits(id) => return sim::session::time_answers::utterance(value),
            Some(id) if sim::session::admits(id) => return sim::session::utterance(value),
            Some("m4-spoof-tool") => return sim::m4::utterance(value),
            Some("m5-twin-room-lights") => return sim::mcp::utterance(value),
            Some("m6-emotion") => return sim::m4::emotion_utterance(value),
            Some("m7-muse") => return sim::m7::utterance(value),
            _ => {}
        }
        return Ok(Step::SayFile(sim::fixture(value, scenario)?));
    }
    if let Some(text) = value.strip_prefix("text:") {
        if options.speech.is_none() || text.trim().is_empty() {
            return Err(
                "say: text needs content and both --speech-endpoint and --speech-model before any effects".into(),
            );
        }
        return Ok(Step::SayText(text.to_owned()));
    }
    if options.bridge_config.is_some() && Path::new(value).extension().is_some_and(|ext| ext == "pcm") {
        return Ok(Step::SayFile(sim::fixture(value, scenario)?));
    }
    let relative = Path::new(value);
    if relative.is_absolute() || relative.extension().is_none_or(|ext| ext != "wav") {
        return Err("say: expected a WAV path relative to the scenario directory".into());
    }
    let path = Path::new(scenario)
        .parent()
        .unwrap_or(Path::new("."))
        .join(relative)
        .canonicalize()
        .map_err(|_| "say: utterance WAV is missing; supply the scenario-relative recording")?;
    if !path.is_file() {
        return Err("say: utterance WAV must be a file".into());
    }
    Ok(Step::SayFile(path))
}

fn load(options: &Options) -> Result<(String, String, Vec<Step>), String> {
    let mut milestone = None;
    let mut steps = Vec::new();
    let mut tier = "T0".to_owned();
    for path in &options.scenarios {
        let scenario = crate::lab_validate::load(path)?;
        let name = scenario["milestone"].as_str().ok_or("scenario: missing milestone")?;
        if milestone.as_ref().is_some_and(|value| value != name) {
            return Err("run: batch scenarios must share a milestone".into());
        }
        milestone = Some(name.to_owned());
        for declared in scenario["tiers"].as_array().ok_or("scenario: missing tiers")? {
            if let Some(value) = declared.as_str()
                && value > tier.as_str()
            {
                value.clone_into(&mut tier);
            }
        }
        if !(options.sim || options.bridge_config.is_some() && device::config::scenario::admits(&scenario)?)
            && (scenario["setup"].as_object().is_none_or(|v| !v.is_empty())
                || scenario["budgets"].as_object().is_none_or(|v| !v.is_empty())
                || scenario["negative"].as_array().is_none_or(|v| !v.is_empty()))
        {
            return Err("run: setup, budgets and injections are not implemented by device driver".into());
        }
        if options.sim {
            sim_setup(&scenario)?;
            tier = "T3".into();
        }
        for step in scenario["steps"].as_array().ok_or("scenario: missing steps")? {
            steps.push(match step["say"].as_str() {
                Some(value) => say(value, path, options)?,
                None if options.sim => sim::parse_step(step, path)?,
                None if options.bridge_config.is_some() || name == "M1" => device::step(step, path, options)?,
                None => run_oracles::parse(step)?,
            });
        }
        if options.sim {
            steps.extend(sim::m4::negative(&scenario)?);
            steps.extend(sim::m2::negative(&scenario)?);
            steps.extend(sim::m7::negative(&scenario)?);
            for (name, value) in scenario["budgets"].as_object().ok_or("sim: missing budgets")? {
                steps.push(Step::Budget(
                    name.clone(),
                    value.as_f64().ok_or("budget: invalid ceiling")?,
                ));
            }
        }
    }
    if steps.is_empty() {
        return Err("run: scenario batch has no executable steps".into());
    }
    Ok((milestone.ok_or("run: missing milestone")?, tier, steps))
}

fn sim_setup(scenario: &serde_json::Value) -> Result<(), String> {
    if !scenario["tiers"]
        .as_array()
        .is_some_and(|v| v.iter().any(|t| t == "T3"))
    {
        return Err("sim: scenario does not admit T3".into());
    }
    if scenario["id"] == "m7-muse" {
        sim::m7::negative(scenario)?;
        return Ok(());
    }
    if scenario["setup"].as_object().is_none_or(|m| {
        m.iter().any(|(k, v)| {
            !(matches!(
                (k.as_str(), v.as_str()),
                ("mcp", Some("spoof")) | ("backend", Some("client"))
            ) || matches!(scenario["id"].as_str(), Some("m5-port-os-link" | "m5-twin-room-lights"))
                && k == "mcp"
                && v == "portos-pilot")
        })
    }) {
        return Err("sim: unsupported setup mode".into());
    }
    if scenario["id"] == "m2-live-connect" {
        sim::m2::negative(scenario)?;
        return Ok(());
    }
    if scenario["id"] == "m4-spoof-tool" {
        sim::m4::negative(scenario)?;
        return Ok(());
    }
    if scenario["negative"].as_array().is_none_or(|v| !v.is_empty()) {
        return Err("sim: unsupported negative injection".into());
    }
    Ok(())
}

fn combine(primary: Result<(), String>, cleanup: Result<(), String>) -> Result<(), String> {
    match (primary, cleanup) {
        (Err(primary), Err(cleanup)) => Err(format!("{primary}; {cleanup}")),
        (primary, cleanup) => primary.and(cleanup),
    }
}

fn dispatch(driver: &mut dyn Driver, steps: &[Step], artifacts: &mut Artifacts) -> Result<(), String> {
    artifacts.event("run_started", None, None)?;
    for (index, step) in steps.iter().enumerate() {
        artifacts.event("step_started", Some(index), None)?;
        let result = match step {
            Step::LinkCounts(expected) => artifacts.expect_link_counts(index, expected),
            _ => driver.execute(step, index, steps.get(index + 1), artifacts),
        };
        let key = format!("step_{index}");
        artifacts
            .checks
            .insert(key.clone(), result.is_ok() && !artifacts.blocked.contains_key(&key));
        if let Err(message) = result {
            artifacts.event("step_failed", Some(index), Some(&message))?;
            return Err(if artifacts.sim {
                format!("step {index}: {message}")
            } else {
                message
            });
        }
        let blocked = artifacts.blocked.get(&key).cloned();
        artifacts.event(
            if blocked.is_some() {
                "step_blocked"
            } else {
                "step_completed"
            },
            Some(index),
            blocked.as_deref(),
        )?;
    }
    Ok(())
}

fn completed(result: Result<(), String>, artifacts: &Artifacts, scenario: Option<&str>) -> Result<(), String> {
    result
        .and_then(|()| {
            if artifacts.blocked.is_empty() {
                Ok(())
            } else {
                Err("M1: mute evidence blocked; see report limitations".into())
            }
        })
        .map_err(|e| scenario.map_or_else(|| e.clone(), |path| format!("{path}: {e}")))
}

fn execute(args: &[String], output: &mut impl Write) -> Result<(), String> {
    execute_with_errors(args, output, &mut std::io::sink())
}

fn execute_with_errors(args: &[String], output: &mut impl Write, errors: &mut impl Write) -> Result<(), String> {
    let options = run_options::parse(args)?;
    let (milestone, tier, steps) = load(&options)?;
    if !options.sim
        && options.bridge.is_none()
        && options.on_device.is_none()
        && steps.iter().any(|step| {
            matches!(
                step,
                Step::Ui(_) | Step::Trace(_) | Step::Press(_) | Step::Restart { .. }
            )
        })
    {
        return Err("device: trace and UiState steps require --bridge and --bridge-config".into());
    }
    let configuration = device::config::load(&options)?;
    let root = run_options::destination(&options.out)?;
    let digest = run_command::helper(&options.python, "digest", &options.scenarios)
        .map_err(|_| "scenario: cannot compute digest")?;
    let digest = digest["sha256"]
        .as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("scenario: invalid SHA-256 digest")?;
    let mut artifacts = Artifacts::create(root)?;
    artifacts.sim = options.sim;
    device::config::scenario::record(&options, configuration.as_deref(), &mut artifacts)?;
    let mut driver = driver(&options, configuration, errors)?;
    let result = dispatch(driver.as_mut(), &steps, &mut artifacts);
    let cleanup = driver.close(&mut artifacts);
    let final_counts = artifacts.finalize_link_counts();
    artifacts.checks.insert(
        if options.sim {
            "world_closed"
        } else {
            "session_released"
        }
        .into(),
        cleanup.is_ok(),
    );
    let result = combine(result, combine(cleanup, final_counts));
    artifacts.event(
        if result.is_ok() { "run_completed" } else { "run_failed" },
        None,
        result.as_ref().err().map(String::as_str),
    )?;
    artifacts.publish(
        &milestone,
        &tier,
        digest,
        result.as_ref().err().map(String::as_str),
        options.keep_audio,
    )?;
    writeln!(
        output,
        "{}: {}",
        artifacts.root.display(),
        artifacts.outcome(result.as_ref().err().map(String::as_str))
    )
    .map_err(|_| "run stdout: write failed")?;
    completed(result, &artifacts, options.sim.then(|| options.scenarios[0].as_str()))
}

pub fn run(args: &[String], output: &mut impl Write, errors: &mut impl Write) -> u8 {
    let result = if args.iter().any(|s| s == "--all") {
        sim::suite::execute(args, output)
    } else {
        execute_with_errors(args, output, errors)
    };
    match result {
        Ok(()) => 0,
        Err(message) => {
            let _ = writeln!(errors, "echo-lab: {message}");
            1
        }
    }
}

fn driver<'a>(
    options: &'a Options,
    configuration: Option<String>,
    errors: &'a mut dyn Write,
) -> Result<Box<dyn Driver + 'a>, String> {
    if !options.sim {
        if let Some(config) = configuration {
            return Ok(Box::new(device::Configured::new(options, config, errors)?));
        }
        if options.wake_clip.is_some() {
            return Ok(Box::new(device::pcm::Standalone::new(options)));
        }
        return Ok(Box::new(run_device::Device::new(options)));
    }
    let scenario = crate::lab_validate::load(&options.scenarios[0])?;
    if scenario["id"] == "m2-live-connect" {
        return Ok(Box::new(sim::m2::M2::new(options)?));
    }
    if matches!(
        scenario["id"].as_str(),
        Some("m3-full-duplex" | "session-barge-continues")
    ) {
        return Ok(Box::new(sim::m3::M3::new(options)?));
    }
    if scenario["id"].as_str().is_some_and(sim::session::goodbye::admits) {
        return Ok(Box::new(sim::session::goodbye::Goodbye::new(options)?));
    }
    if scenario["id"].as_str().is_some_and(sim::session::time_answers::admits) {
        return Ok(Box::new(sim::session::time_answers::TimeAnswers::new(options)?));
    }
    if scenario["id"].as_str().is_some_and(sim::session::admits) {
        return Ok(Box::new(sim::session::Persistent::new(options)?));
    }
    if scenario["milestone"] == "M5" {
        return Ok(Box::new(sim::mcp::M5::new(
            options,
            scenario["id"] == "m5-twin-room-lights",
        )?));
    }
    if scenario["id"] == "m7-muse" {
        return Ok(Box::new(sim::m7::M7::new(options)?));
    }
    if matches!(scenario["id"].as_str(), Some("m4-spoof-tool" | "m6-emotion")) {
        Ok(Box::new(sim::m4::M4::new(options)?))
    } else {
        Ok(Box::new(sim::Sim::new(options)?))
    }
}

#[cfg(test)]
mod setup_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mcp_pilot_setup_selects_local_peers_only_for_common_m5() {
        let mut scenario = json!({"id":"m5-port-os-link","tiers":["T3","T4","T5"],
            "setup":{"mcp":"portos-pilot","backend":"client"},"negative":[]});
        assert_eq!(sim_setup(&scenario), Ok(()));
        scenario["setup"]["mcp"] = json!("spoof");
        assert_eq!(sim_setup(&scenario), Ok(()));
        scenario["setup"]["mcp"] = json!("portos-pilot");
        for id in ["m1-wake-stream", "m4-spoof-tool", "other"] {
            scenario["id"] = json!(id);
            assert_eq!(sim_setup(&scenario), Err("sim: unsupported setup mode".into()));
        }
        scenario["id"] = json!("m5-port-os-link");
        scenario["setup"]["backend"] = json!("muse");
        assert_eq!(sim_setup(&scenario), Err("sim: unsupported setup mode".into()));
    }
}
