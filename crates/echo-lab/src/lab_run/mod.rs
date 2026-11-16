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
    if options.sim {
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
        if !options.sim
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
                None => run_oracles::parse(step)?,
            });
        }
        if options.sim {
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
    if scenario["setup"].as_object().is_none_or(|m| {
        m.iter().any(|(k, v)| {
            !matches!(
                (k.as_str(), v.as_str()),
                ("mcp", Some("spoof")) | ("backend", Some("client"))
            )
        })
    }) {
        return Err("sim: unsupported setup mode".into());
    }
    if scenario["negative"].as_array().is_none_or(|v| !v.is_empty()) {
        return Err("sim: unsupported negative injection".into());
    }
    Ok(())
}

fn dispatch(driver: &mut dyn Driver, steps: &[Step], artifacts: &mut Artifacts) -> Result<(), String> {
    artifacts.event("run_started", None, None)?;
    for (index, step) in steps.iter().enumerate() {
        artifacts.event("step_started", Some(index), None)?;
        let result = driver.execute(step, index, steps.get(index + 1), artifacts);
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
    let options = run_options::parse(args)?;
    let (milestone, tier, steps) = load(&options)?;
    let root = run_options::destination(&options.out)?;
    let digest = run_command::helper(&options.python, "digest", &options.scenarios)
        .map_err(|_| "scenario: cannot compute digest")?;
    let digest = digest["sha256"]
        .as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("scenario: invalid SHA-256 digest")?;
    let mut artifacts = Artifacts::create(root)?;
    artifacts.sim = options.sim;
    let mut driver: Box<dyn Driver> = if options.sim {
        Box::new(sim::Sim::new(&options)?)
    } else {
        Box::new(run_device::Device::new(&options))
    };
    let result = dispatch(driver.as_mut(), &steps, &mut artifacts);
    let cleanup = driver.close(&mut artifacts);
    artifacts.checks.insert(
        if options.sim {
            "world_closed"
        } else {
            "session_released"
        }
        .into(),
        cleanup.is_ok(),
    );
    let result = match (result, cleanup) {
        (Err(primary), Err(cleanup)) => Err(format!("{primary}; {cleanup}")),
        (primary, cleanup) => primary.and(cleanup),
    };
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
        execute(args, output)
    };
    match result {
        Ok(()) => 0,
        Err(message) => {
            let _ = writeln!(errors, "echo-lab: {message}");
            1
        }
    }
}
