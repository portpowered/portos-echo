//! Continuous M3 scenario, using the delivered bridge and native host only.
mod live;
mod oracle;
mod wire;
mod world;
use super::{controlled, selectors};
use crate::lab_run::{Driver, Step, run_options::Options, run_report::Artifacts};
use serde_json::Value;

pub const FIRST: &str = "Computer, what time is it?";
pub const SECOND: &str = "Computer, what day is it?";

pub fn utterance(value: &str) -> Result<Step, String> {
    if !matches!(value, FIRST | SECOND) {
        return Err("M3: no synthetic marker for this utterance".into());
    }
    Ok(Step::SayText(value.into()))
}

pub struct M3<'a> {
    options: &'a Options,
    runtime: tokio::runtime::Runtime,
    world: world::World,
    launched: bool,
    deadline: tokio::time::Instant,
    traces: Vec<(Vec<Value>, bool)>,
}

impl<'a> M3<'a> {
    pub fn new(options: &'a Options) -> Result<Self, String> {
        let scenario = crate::lab_validate::load(&options.scenarios[0])?;
        if scenario["budgets"] != serde_json::json!({"reply_send_window_ms":200,"barge_in_cut_ms":600}) {
            return Err("M3: requires reply_send_window_ms=200 and strict barge_in_cut_ms=600".into());
        }
        Ok(Self {
            options,
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|_| "M3: cannot start runtime")?,
            world: world::World::default(),
            launched: false,
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(120),
            traces: vec![],
        })
    }

    async fn step(world: &mut world::World, step: &Step) -> Result<(), String> {
        match step {
            Step::SayText(text) if text == FIRST && world.first_pcm.is_none() => world.first().await,
            Step::SayText(text) if text == SECOND && world.first_pcm.is_some() && world.second_pcm.is_none() => {
                world.second().await
            }
            Step::SayText(text)
                if text == FIRST && world.muted_live_before.is_some() && world.muted_live_after.is_none() =>
            {
                world.muted_input().await
            }
            Step::Press(action) if action == "mute" => world.press().await,
            Step::Ui(expected) => {
                world
                    .screen
                    .as_mut()
                    .ok_or("M3: missing screen")?
                    .expect(expected)
                    .await
            }
            Step::Trace(values) => {
                world.read_trace()?;
                selectors::check(&world.trace.records, &selectors::parse(values)?, Some("run-1"))
            }
            Step::Speech(phrases) => {
                let live = world.live.as_ref().ok_or("M3: missing Live")?.observed.borrow();
                crate::lab_run::run_oracles::speech(
                    live.transcript
                        .as_deref()
                        .ok_or("M3: missing sent transcript")?
                        .as_bytes(),
                    phrases,
                )
            }
            Step::Budget(name, value) if name == "reply_send_window_ms" && value.to_bits() == 200.0_f64.to_bits() => {
                Ok(())
            }
            Step::Budget(name, value) if name == "barge_in_cut_ms" && value.to_bits() == 600.0_f64.to_bits() => {
                let state = world.relay.as_ref().ok_or("M3: missing wire")?.observed.borrow();
                oracle::duplex(&state, world.first_pcm, world.second_pcm)?;
                oracle::muted(
                    &world.trace.records,
                    world.muted_live_before.ok_or("M3 mute: missing native open window")?,
                    world.muted_live_after.ok_or("M3 mute: missing native closed window")?,
                )
                .map(|_| ())
            }
            _ => Err("M3: unsupported step or invalid conversation phase".into()),
        }
    }
}

impl Driver for M3<'_> {
    fn execute(&mut self, step: &Step, _: usize, _: Option<&Step>, artifacts: &mut Artifacts) -> Result<(), String> {
        self.runtime.block_on(controlled(
            async {
                if !self.launched {
                    self.launched = true;
                    self.world
                        .start(
                            &self.options.python,
                            self.options.bridge.as_deref().ok_or("M3: missing production bridge")?,
                            &artifacts.root.join("m3.private"),
                        )
                        .await?;
                }
                Self::step(&mut self.world, step).await
            },
            self.options.cancel.as_deref(),
            self.deadline,
        ))?;
        if let Step::Trace(values) = step {
            self.traces.push((values.clone(), self.world.second_pcm.is_none()));
        }
        Ok(())
    }

    fn close(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
        let cleanup = self.runtime.block_on(self.world.close());
        for address in &self.world.endpoints {
            artifacts.endpoint(*address)?;
        }
        for record in &self.world.trace.records {
            artifacts.observation(record)?;
        }
        if let Some(state) = &self.world.final_wire {
            artifacts.event("m3.wire", None, Some(&format!("runs={} inputs={} Live-frames={:?} reply_bytes={:?} stops={} Played={} cut_order={:?} first_host_pcm={:?} second_host_pcm={:?}", state.runs, state.inputs, self.world.muted_live_before, state.bytes, state.stops, state.played.len(), state.cut_order, self.world.first_pcm, self.world.second_pcm)))?;
        }
        cleanup?;
        for (values, first) in &self.traces {
            let records = &self.world.trace.records;
            let end = if *first {
                records
                    .iter()
                    .position(|r| r["event"] == "reply.played")
                    .map_or(records.len(), |index| index + 1)
            } else {
                records.len()
            };
            selectors::check(&records[..end], &selectors::parse(values)?, Some("run-1"))?;
        }
        if self.world.second_pcm.is_some() {
            let summary = oracle::duplex(
                self.world
                    .final_wire
                    .as_ref()
                    .ok_or("M3: missing final wire evidence")?,
                self.world.first_pcm,
                self.world.second_pcm,
            )?;
            artifacts.event("m3.duplex", None, Some(&summary))?;
            artifacts.checks.insert("m3_duplex".into(), true);
        }
        if let (Some(before), Some(after)) = (self.world.muted_live_before, self.world.muted_live_after) {
            let summary = oracle::muted(&self.world.trace.records, before, after)?;
            artifacts.event("m3.mute", None, Some(&summary))?;
            artifacts.checks.insert("m3_mute".into(), true);
        }
        Ok(())
    }
}
