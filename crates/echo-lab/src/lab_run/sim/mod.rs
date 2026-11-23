pub mod m2;
pub mod m3;
pub mod m4;
pub mod m7;
pub(super) mod mcp;
pub(super) mod observers;
pub(super) mod process;
pub(super) mod production;
pub(super) mod selectors;
pub mod session;
pub mod suite;
pub(super) mod trace;
mod world;

use std::path::PathBuf;
use std::time::Duration;

use super::{Driver, Step, run_options::Options, run_report::Artifacts};
use serde_json::{Value, json};
use world::World;

pub struct Sim<'a> {
    options: &'a Options,
    runtime: tokio::runtime::Runtime,
    world: World,
    launched: bool,
    expectations: Vec<(usize, Vec<selectors::Selector>, Option<String>, u64)>,
    budgets: Vec<(usize, String, f64, Option<String>, u64)>,
    deadline: tokio::time::Instant,
    m1: bool,
    wifi: bool,
    mute: production::mute::Probe,
}

impl<'a> Sim<'a> {
    pub fn new(options: &'a Options) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| "sim: cannot start runtime")?;
        let scenarios: Vec<Value> = options
            .scenarios
            .iter()
            .filter_map(|path| crate::lab_validate::load(path).ok())
            .collect();
        Ok(Self {
            m1: scenarios.iter().any(|s| s["id"] == "m1-hassmic-bridge"),
            wifi: scenarios.iter().any(|s| s["id"] == production::wifi::ID),
            deadline: tokio::time::Instant::now() + run_budget(&scenarios),
            options,
            mute: production::mute::Probe::default(),
            runtime,
            world: World::default(),
            launched: false,
            expectations: vec![],
            budgets: vec![],
        })
    }

    fn attempt(&mut self, step: &Step, artifacts: &Artifacts) -> Result<(), String> {
        if let Step::Mcp(action) = step {
            return Err(format!(
                "sim: MCP {} control requires the M5 bridge driver",
                action.name()
            ));
        }
        self.runtime.block_on(async {
            let work = async {
                if !self.launched {
                    self.launched = true;
                    let bridge = self.options.bridge.as_deref().ok_or("sim: missing bridge")?;
                    if self.wifi {
                        self.world.wifi = Some(production::wifi::Device::start(bridge).await?);
                    }
                    self.world.start(&self.options.python, bridge, &artifacts.root).await?;
                }
                if let Step::Press(action) = step
                    && self.wifi
                    && production::wifi::action(action)
                {
                    let result = observe_wifi(&mut self.world, action).await?;
                    return artifacts.measurement(json!({"wifi_step":action,"result":result}));
                }
                if self.m1 && (self.mute.skipped(step) || self.mute.observe(&mut self.world, step).await?) {
                    return Ok(());
                }
                execute_step(&mut self.world, step, self.options, &artifacts.root).await?;
                self.mute.pressed(step);
                Ok(())
            };
            controlled(work, self.options.cancel.as_deref(), self.deadline).await
        })
    }

    fn finalize(&self, artifacts: &mut Artifacts, invalid_trace: bool) -> Vec<String> {
        let mut failures = Vec::new();
        for (step, selectors, run, generation) in &self.expectations {
            let result = if invalid_trace {
                Err("trace: invalid final trace".into())
            } else {
                selectors::check(self.world.records(*generation), selectors, run.as_deref())
            };
            if let Err(error) = result {
                artifacts.checks.insert(format!("step_{step}"), false);
                failures.push(format!("step {step}: {error}"));
            }
        }
        for (step, name, ceiling, run, generation) in &self.budgets {
            let result = if invalid_trace {
                Err("trace: invalid final trace".into())
            } else {
                observers::budget(self.world.records(*generation), name, *ceiling, run.as_deref())
            };
            if let Err(error) = result {
                artifacts.checks.insert(format!("step_{step}"), false);
                failures.push(format!("step {step}: {error}"));
            }
        }
        failures
    }
}

async fn observe_wifi(world: &mut World, action: &str) -> Result<Value, String> {
    let address = *world.endpoints.last().ok_or("Wi-Fi: screen address missing")?;
    let result = world
        .wifi
        .as_ref()
        .ok_or("Wi-Fi: device not started")?
        .perform(action, address)
        .await?;
    if action == "wifi-lose" {
        world.control(&json!({"action":"wake"})).await?;
        world
            .screen
            .as_mut()
            .ok_or("Wi-Fi: screen not started")?
            .expect(&json!({"state":"error"}))
            .await?;
    }
    Ok(result)
}

async fn execute_step(world: &mut World, step: &Step, options: &Options, root: &std::path::Path) -> Result<(), String> {
    match step {
        Step::Restart {
            process,
            count,
            say: path,
        } => {
            production::restart::group(
                world,
                process,
                *count,
                path,
                options.bridge.as_deref().ok_or("sim: missing bridge")?,
                root,
            )
            .await
        }
        Step::SayFile(path) => {
            let data = observers::bytes(path, 4096, "say")?;
            if data.is_empty() || data.len() % 2 != 0 {
                return Err("say: invalid PCM fixture".into());
            }
            say(world, &data).await
        }
        Step::Press(action) => {
            world.control(&json!({"action":action})).await?;
            if matches!(action.as_str(), "wake" | "action") {
                world.complete_run().await?;
            }
            Ok(())
        }
        Step::Trace(values) => {
            world.observe().await?;
            selectors::check(&world.trace.records, &selectors::parse(values)?, world.run.as_deref())
        }
        Step::Ui(expected) => {
            if let Some(screen) = &mut world.screen {
                return screen.expect(expected).await;
            }
            let bytes = observers::bytes(&world.ui_path, 65536, "screen expectation")?;
            observers::screen(&bytes, expected)
        }
        Step::Speech(phrases) => {
            world.observe().await?;
            let text = observers::transcript(&world.trace.records, world.run.as_deref())?;
            super::run_oracles::speech(text.as_bytes(), phrases)
        }
        Step::Budget(name, ceiling) => {
            world.observe().await?;
            observers::budget(&world.trace.records, name, *ceiling, world.run.as_deref())
        }
        _ => Err("sim: unsupported step capability".into()),
    }
}

async fn say(world: &mut World, data: &[u8]) -> Result<(), String> {
    world.control(&json!({"action":"say", "pcm":data})).await?;
    if world.screen.as_ref().is_some_and(production::Screen::muted) {
        // Capture consumption is acknowledged by the host. Only the explicit
        // bridge mute-window measurement can prove received/forwarded zero.
        world.observe().await
    } else {
        world.complete_run().await
    }
}

async fn controlled<T>(
    work: impl std::future::Future<Output = Result<T, String>>,
    cancel: Option<&std::path::Path>,
    deadline: tokio::time::Instant,
) -> Result<T, String> {
    // The cancel file is checked before every poll of the work, so a stage
    // makes no further progress once the file exists. The 10 ms timer only
    // wakes a stage that is otherwise idle; it is not the cancellation latency.
    let mut tick = std::pin::pin!(tokio::time::sleep(Duration::from_millis(10)));
    let cancellation = std::future::poll_fn(|context| {
        loop {
            if cancel.is_some_and(std::path::Path::exists) {
                return std::task::Poll::Ready(());
            }
            if cancel.is_none() || std::future::Future::poll(tick.as_mut(), context).is_pending() {
                return std::task::Poll::Pending;
            }
            tick.as_mut()
                .reset(tokio::time::Instant::now() + Duration::from_millis(10));
        }
    });
    tokio::select! {
        biased;
        () = cancellation => Err("sim: cancelled".into()),
        result = tokio::time::timeout_at(deadline, work) => result.map_err(|_| "sim: stage timed out".to_owned())?,
    }
}

impl Driver for Sim<'_> {
    fn execute(
        &mut self,
        step: &Step,
        index: usize,
        _: Option<&Step>,
        artifacts: &mut Artifacts,
    ) -> Result<(), String> {
        if let Err(error) = self.attempt(step, artifacts) {
            if self.world.bridge.is_none() {
                return Err(error);
            }
            let diagnostics = self.runtime.block_on(self.world.failure_diagnostics());
            return Err(format!("{error}; {diagnostics}"));
        }
        if self.m1 && self.mute.skipped(step) {
            artifacts
                .blocked
                .insert(format!("step_{index}"), production::mute::LIMITATION.into());
            return Ok(());
        }
        if let Step::Trace(values) = step {
            self.expectations.push((
                index,
                selectors::parse(values)?,
                self.world.run.clone(),
                self.world.generation,
            ));
        }
        if let Step::Budget(name, ceiling) = step {
            self.budgets.push((
                index,
                name.clone(),
                *ceiling,
                self.world.run.clone(),
                self.world.generation,
            ));
        }
        Ok(())
    }

    fn close(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
        let cleanup = self.runtime.block_on(self.world.close());
        let invalid_trace = cleanup.as_ref().err().is_some_and(|e| e.contains("trace:"));
        let mut failures = cleanup.err().into_iter().collect::<Vec<_>>();
        for address in &self.world.endpoints {
            if let Err(error) = artifacts.endpoint(*address) {
                failures.push(error);
                break;
            }
        }
        for record in self
            .world
            .history
            .iter()
            .flat_map(|t| &t.records)
            .chain(&self.world.trace.records)
            .filter(|_| !invalid_trace)
        {
            if let Err(error) = artifacts.observation(record) {
                failures.push(error);
                break;
            }
        }
        failures.extend(self.finalize(artifacts, invalid_trace));
        if self.m1 && self.world.initial_ready.is_some() {
            let m1 = production::report::collect(&self.world);
            let checked = production::report::check(&self.world, &m1);
            artifacts
                .checks
                .insert("m1_counts".into(), checked.is_ok() && m1["mute_status"] == "pass");
            failures.extend(checked.err());
            artifacts.m1 = Some(m1);
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

/// The deadline for a whole sim run: a base for the scenario's ordinary steps
/// plus one allowance per declared restart cycle. A cycle stops a process,
/// starts it, re-admits it and completes a wake, about 4.5 s on a quiet host
/// and up to 6 s on a hosted runner; M1 declares twenty of them, so a fixed
/// 120 s left it with almost no headroom there.
pub(super) fn run_budget(scenarios: &[Value]) -> Duration {
    const BASE: Duration = Duration::from_secs(120);
    const PER_RESTART: Duration = Duration::from_secs(10);
    let cycles: u64 = scenarios
        .iter()
        .filter_map(|scenario| scenario["steps"].as_array())
        .flatten()
        .filter_map(|step| step["restart"]["count"].as_u64())
        .sum();
    BASE.saturating_add(PER_RESTART.saturating_mul(u32::try_from(cycles).unwrap_or(u32::MAX)))
}

pub fn parse_step(value: &Value, scenario: &str) -> Result<Step, String> {
    if let Some(restart) = value.get("restart") {
        let process = restart["process"]
            .as_str()
            .filter(|p| matches!(*p, "bridge" | "hassmic-host"))
            .ok_or("restart: unsupported process")?;
        let count = restart["count"]
            .as_u64()
            .filter(|n| (1..=10).contains(n))
            .ok_or("restart: count must be 1..10")?;
        return Ok(Step::Restart {
            process: process.into(),
            count,
            say: fixture(restart["say"].as_str().ok_or("restart: missing say fixture")?, scenario)?,
        });
    }
    if let Some(action) = value["press"].as_str()
        && production::wifi::action(action)
        && crate::lab_validate::load(scenario)?["id"] == production::wifi::ID
    {
        return Ok(Step::Press(action.into()));
    }
    parse(value)
}

pub fn parse(value: &Value) -> Result<Step, String> {
    if let Some(values) = value["expect_trace"].as_array() {
        selectors::parse(values)?;
        return Ok(Step::Trace(values.clone()));
    }
    if let Some(press) = value.get("press") {
        let action = press.as_str().or_else(|| {
            press
                .as_object()
                .filter(|m| m.len() == 1)
                .and_then(|m| m.get("key"))
                .and_then(Value::as_str)
        });
        return match action {
            Some(action @ ("wake" | "action" | "pair" | "stop" | "mute")) => Ok(Step::Press(action.into())),
            _ => Err("sim: unsupported press control".into()),
        };
    }
    if value["expect_screen"].is_object() {
        return Ok(Step::Ui(value["expect_screen"].clone()));
    }
    super::run_oracles::parse(value)
}

pub fn fixture(value: &str, scenario: &str) -> Result<PathBuf, String> {
    let relative = std::path::Path::new(value);
    if relative.is_absolute() || relative.extension().is_none_or(|e| e != "pcm") {
        return Err("sim say: expected scenario-relative s16le mono 16000 Hz PCM".into());
    }
    let path = std::path::Path::new(scenario)
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join(relative)
        .canonicalize()
        .map_err(|_| "sim say: missing PCM fixture")?;
    let size = std::fs::metadata(&path)
        .map_err(|_| "sim say: cannot stat fixture")?
        .len();
    if size == 0 || size > 4096 || size % 2 != 0 {
        return Err("sim say: fixture must contain 1..2048 complete samples".into());
    }
    Ok(path)
}

#[cfg(test)]
mod startup_tests;
#[cfg(test)]
mod tests;
