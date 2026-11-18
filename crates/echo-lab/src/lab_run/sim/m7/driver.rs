use super::super::{controlled, observers, selectors};
use super::{muse, world::World};
use crate::lab_run::{Driver, Step, run_options::Options, run_report::Artifacts};
use serde_json::{Value, json};
use std::time::Duration;

pub struct M7<'a> {
    options: &'a Options,
    runtime: tokio::runtime::Runtime,
    binaries: std::path::PathBuf,
    world: World,
    measured: Option<Value>,
    negative: bool,
}

impl<'a> M7<'a> {
    pub fn new(options: &'a Options) -> Result<Self, String> {
        // All prebuilt prerequisites are checked before reading private files or
        // starting loopback peers, including when --bridge is explicit.
        let binaries = muse::prebuilt()?;
        Ok(Self {
            options,
            binaries,
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|_| "m7: runtime unavailable")?,
            world: World::default(),
            measured: None,
            negative: false,
        })
    }

    fn journey(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
        let previous = self.measured.take();
        self.world = World::default();
        let case = if self.negative { "no_report" } else { "success" };
        let work = self.runtime.block_on(controlled(
            async {
                self.world
                    .start(
                        &self.options.python,
                        self.options.bridge.as_deref().ok_or("m7: missing production bridge")?,
                        &artifacts.root.join(format!("private-{case}")),
                        self.negative,
                        &self.binaries,
                    )
                    .await?;
                self.world.speak(self.negative).await
            },
            self.options.cancel.as_deref(),
            tokio::time::Instant::now() + Duration::from_secs(30),
        ));
        let cleanup = self.runtime.block_on(self.world.close());
        // Keep only production's identity-only trace, including partial failures.
        let name = format!("m7-{case}.jsonl");
        let records = self
            .world
            .trace
            .records
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(artifacts.root.join(&name), records + "\n").map_err(|_| "m7: trace retention failed")?;
        artifacts.evidence.push(name);
        match (work, cleanup) {
            (Err(primary), Err(cleanup)) => return Err(format!("{primary}; {cleanup}")),
            (Err(error), _) | (_, Err(error)) => return Err(error),
            _ => {}
        }
        let mut value = self.world.observation(self.negative)?;
        let audio_name = format!("m7-{case}-audible.json");
        let audio = json!({"audibility":value["audibility"],"ui_states":value["ui_states"]});
        std::fs::write(artifacts.root.join(&audio_name), audio.to_string())
            .map_err(|_| "m7: audio retention failed")?;
        artifacts.evidence.push(audio_name);
        if let Some(object) = value.as_object_mut() {
            object.remove("audibility");
            object.remove("ui_states");
        }
        if let Some(previous) = previous {
            for field in ["task_id", "gadget_session_id"] {
                if previous[field] == value[field] {
                    return Err(format!("m7: fresh world reused {field}"));
                }
            }
        }
        artifacts.measurement(value.clone())?;
        self.measured = Some(value);
        Ok(())
    }

    fn check(&self, step: &Step) -> Result<(), String> {
        let measured = self.measured.as_ref().ok_or("m7: missing current-case observation")?;
        let records = &self.world.trace.records;
        let run = measured["run_id"].as_str();
        match step {
            Step::Trace(values) => selectors::check(records, &selectors::parse(values)?, run),
            Step::TraceAbsent(values) => {
                for value in values {
                    if selectors::check(records, &selectors::parse(std::slice::from_ref(value))?, run).is_ok() {
                        return Err("m7: forbidden trace observed".into());
                    }
                }
                Ok(())
            }
            Step::Speech(phrases) => crate::lab_run::run_oracles::speech(
                measured["transcript"]
                    .as_str()
                    .ok_or("m7: missing transcript")?
                    .as_bytes(),
                phrases,
            ),
            Step::Ui(expected) => observers::screen(
                &serde_json::to_vec(self.world.speaking.as_ref().ok_or("m7: missing screen observation")?)
                    .map_err(|_| "m7: invalid screen")?,
                expected,
            ),
            _ => Err("m7: unsupported scenario step".into()),
        }
    }
}

impl Driver for M7<'_> {
    fn execute(&mut self, step: &Step, _: usize, _: Option<&Step>, artifacts: &mut Artifacts) -> Result<(), String> {
        match step {
            Step::SayText(text) if self.measured.is_none() && !self.negative => {
                super::utterance(text)?;
                self.journey(artifacts)
            }
            Step::Inject if self.measured.is_some() && !self.negative => {
                self.negative = true;
                self.journey(artifacts)
            }
            _ => self.check(step),
        }
    }

    fn close(&mut self, _: &mut Artifacts) -> Result<(), String> {
        self.runtime.block_on(self.world.close())
    }
}
