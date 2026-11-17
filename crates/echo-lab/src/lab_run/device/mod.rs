//! Opt-in configured device journey. Legacy B0 stays in `run_device`.
pub mod config;
mod confirmation;
mod observations;
pub mod pcm;
mod runtime;

use super::{Driver, Step, run_device::Device, run_options::Options, run_report::Artifacts};

pub struct Configured<'a> {
    options: &'a Options,
    errors: &'a mut dyn std::io::Write,
    presses: u64,
    bench: Device<'a>,
    executor: tokio::runtime::Runtime,
    state: runtime::Runtime,
    config: String,
}

impl<'a> Configured<'a> {
    pub fn new(options: &'a Options, config: String, errors: &'a mut dyn std::io::Write) -> Result<Self, String> {
        Ok(Self {
            options,
            errors,
            presses: 0,
            bench: Device::new(options),
            executor: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|_| "device: cannot start runtime")?,
            state: runtime::Runtime::default(),
            config,
        })
    }

    fn start(&mut self, artifacts: &Artifacts) -> Result<(), String> {
        self.bench.firmware()?;
        if !self.state.started {
            self.executor.block_on(runtime::controlled(
                self.state.start(
                    self.options.bridge.as_deref().ok_or("device: missing bridge")?,
                    &self.config,
                    &artifacts.root,
                ),
                self.options.cancel.as_deref(),
            ))?;
        }
        Ok(())
    }
}

impl Driver for Configured<'_> {
    fn execute(
        &mut self,
        step: &Step,
        index: usize,
        next: Option<&Step>,
        artifacts: &mut Artifacts,
    ) -> Result<(), String> {
        self.start(artifacts)?;
        match step {
            Step::Press(key) if key == "mute" => {
                self.state.arm_mute()?;
                let elapsed = confirmation::wait(
                    self.options.operator_confirm.as_deref(),
                    self.options.cancel.as_deref(),
                    self.errors,
                )?;
                self.presses += 1;
                self.state.confirmed(index, self.presses, elapsed, artifacts)?;
                let expected = if self.presses % 2 == 1 {
                    serde_json::json!({"state":"muted"})
                } else {
                    serde_json::json!({"state":"ready","message":"Echo Live ready"})
                };
                self.executor.block_on(runtime::controlled(
                    self.state.ui(&expected, index, artifacts),
                    self.options.cancel.as_deref(),
                ))
            }
            Step::Ui(value) => self.executor.block_on(runtime::controlled(
                self.state.ui(value, index, artifacts),
                self.options.cancel.as_deref(),
            )),
            Step::Trace(values) => self.state.expect_trace(values, index),
            Step::SayFile(path) => {
                let path = pcm::adapt(path, &artifacts.root, index)?;
                self.bench.execute(&Step::SayFile(path), index, next, artifacts)?;
                if self.state.muted() {
                    return Ok(());
                }
                self.executor.block_on(runtime::controlled(
                    self.state.complete(index),
                    self.options.cancel.as_deref(),
                ))
            }
            _ => self.bench.execute(step, index, next, artifacts),
        }
    }

    fn close(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
        let bench = self.bench.close(artifacts);
        let root = artifacts.root.clone();
        let owned = self.executor.block_on(self.state.close(&root, artifacts));
        super::combine(bench, owned)
    }
}

pub fn step(value: &serde_json::Value) -> Result<Step, String> {
    if value["expect_screen"].get("state").is_some()
        || value.get("expect_trace").is_some()
        || value.get("press").is_some()
    {
        return super::sim::parse(value);
    }
    super::run_oracles::parse(value)
}
