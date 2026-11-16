mod observers;
mod process;
mod production;
mod selectors;
pub mod suite;
mod trace;
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
    expectations: Vec<(usize, Vec<selectors::Selector>, Option<String>)>,
    budgets: Vec<(usize, String, f64, Option<String>)>,
    deadline: tokio::time::Instant,
}

impl<'a> Sim<'a> {
    pub fn new(options: &'a Options) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| "sim: cannot start runtime")?;
        Ok(Self {
            options,
            runtime,
            world: World::default(),
            launched: false,
            expectations: vec![],
            budgets: vec![],
            deadline: tokio::time::Instant::now() + Duration::from_secs(120),
        })
    }

    fn attempt(&mut self, step: &Step, artifacts: &Artifacts) -> Result<(), String> {
        self.runtime.block_on(async {
            let work = async {
                if !self.launched {
                    self.launched = true;
                    self.world
                        .start(
                            &self.options.python,
                            self.options.bridge.as_deref().ok_or("sim: missing bridge")?,
                            &artifacts.root,
                        )
                        .await?;
                }
                match step {
                    Step::SayFile(path) => {
                        let data = observers::bytes(path, 4096, "say")?;
                        if data.is_empty() || data.len() % 2 != 0 {
                            return Err("say: invalid PCM fixture".into());
                        }
                        say(&mut self.world, &data).await
                    }
                    Step::Press(action) => {
                        self.world.control(&json!({"action":action})).await?;
                        if matches!(action.as_str(), "wake" | "action") {
                            self.world.complete_run().await?;
                        }
                        Ok(())
                    }
                    Step::Trace(values) => {
                        self.world.observe().await?;
                        selectors::check(
                            &self.world.trace.records,
                            &selectors::parse(values)?,
                            self.world.run.as_deref(),
                        )
                    }
                    Step::Ui(expected) => {
                        if let Some(screen) = &mut self.world.screen {
                            return screen.expect(expected).await;
                        }
                        let bytes = observers::bytes(&self.world.ui_path, 65536, "screen expectation")?;
                        observers::screen(&bytes, expected)
                    }
                    Step::Speech(phrases) => {
                        self.world.observe().await?;
                        let text = observers::transcript(&self.world.trace.records, self.world.run.as_deref())?;
                        super::run_oracles::speech(text.as_bytes(), phrases)
                    }
                    Step::Budget(name, ceiling) => {
                        self.world.observe().await?;
                        observers::budget(&self.world.trace.records, name, *ceiling, self.world.run.as_deref())
                    }
                    _ => Err("sim: unsupported step capability".into()),
                }
            };
            controlled(work, self.options.cancel.as_deref(), self.deadline).await
        })
    }

    fn finalize(&self, artifacts: &mut Artifacts, invalid_trace: bool) -> Vec<String> {
        let mut failures = Vec::new();
        for (step, selectors, run) in &self.expectations {
            let result = if invalid_trace {
                Err("trace: invalid final trace".into())
            } else {
                selectors::check(&self.world.trace.records, selectors, run.as_deref())
            };
            if let Err(error) = result {
                artifacts.checks.insert(format!("step_{step}"), false);
                failures.push(format!("step {step}: {error}"));
            }
        }
        for (step, name, ceiling, run) in &self.budgets {
            let result = if invalid_trace {
                Err("trace: invalid final trace".into())
            } else {
                observers::budget(&self.world.trace.records, name, *ceiling, run.as_deref())
            };
            if let Err(error) = result {
                artifacts.checks.insert(format!("step_{step}"), false);
                failures.push(format!("step {step}: {error}"));
            }
        }
        failures
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
    let cancellation = async {
        loop {
            if cancel.is_some_and(std::path::Path::exists) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
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
        self.attempt(step, artifacts)?;
        if let Step::Trace(values) = step {
            self.expectations
                .push((index, selectors::parse(values)?, self.world.run.clone()));
        }
        if let Step::Budget(name, ceiling) = step {
            self.budgets
                .push((index, name.clone(), *ceiling, self.world.run.clone()));
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
        for record in self.world.trace.records.iter().filter(|_| !invalid_trace) {
            if let Err(error) = artifacts.observation(record) {
                failures.push(error);
                break;
            }
        }
        failures.extend(self.finalize(artifacts, invalid_trace));
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
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
