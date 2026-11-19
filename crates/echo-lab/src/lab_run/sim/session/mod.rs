//! Persistent voice session journeys on the delivered bridge and native host:
//! listening between replies, idle expiry, and the agent's session tools.
mod oracle;
mod provider;
mod world;
use super::{controlled, selectors};
use crate::lab_run::{Driver, Step, run_options::Options, run_report::Artifacts};
use serde_json::{Value, json};

pub const FIRST: &str = super::m3::FIRST;
pub const SECOND: &str = super::m3::SECOND;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Journey {
    Turns,
    Idle,
    Extend,
    LongWait,
    End,
}

const JOURNEYS: [(&str, Journey); 5] = [
    ("session-two-turns", Journey::Turns),
    ("session-idle-timeout", Journey::Idle),
    ("session-extend-capped", Journey::Extend),
    ("session-extend-reconnected", Journey::LongWait),
    ("session-end-after-reply", Journey::End),
];

fn journey(id: &str) -> Option<Journey> {
    JOURNEYS
        .iter()
        .find(|(name, _)| *name == id)
        .map(|(_, journey)| *journey)
}

pub fn admits(id: &str) -> bool {
    journey(id).is_some()
}

pub fn utterance(value: &str) -> Result<Step, String> {
    if !matches!(value, FIRST | SECOND) {
        return Err("session: no synthetic marker for this utterance".into());
    }
    Ok(Step::SayText(value.into()))
}

impl Journey {
    /// Each delegated turn: scripted session-tool calls, then one answer.
    fn script(self) -> Value {
        match self {
            Self::Turns => json!([[{"say":"It is noon."}], [{"say":"It is Tuesday."}]]),
            Self::Idle => json!([[{"say":"It is noon."}]]),
            Self::Extend | Self::LongWait => json!([[
                {"call":"extend_session","arguments":{"seconds":100}},
                {"call":"extend_session","arguments":{"seconds":100}},
                {"say":"I will keep listening."}
            ]]),
            Self::End => json!([[{"call":"end_session","arguments":{}}, {"say":"Goodbye."}]]),
        }
    }

    /// The configured run cap, when the journey changes the example default.
    fn max(self) -> Option<u64> {
        match self {
            Self::Extend => Some(12),
            Self::LongWait => Some(60),
            _ => None,
        }
    }

    fn budgets(self) -> Value {
        match self.max() {
            Some(max) => json!({"session_listen_seconds":3,"session_max_seconds":max}),
            None => json!({"session_listen_seconds":3}),
        }
    }

    fn utterances(self) -> usize {
        if self == Self::Turns { 2 } else { 1 }
    }
}

pub struct Persistent<'a> {
    options: &'a Options,
    journey: Journey,
    runtime: tokio::runtime::Runtime,
    world: world::World,
    launched: bool,
    deadline: tokio::time::Instant,
    traces: Vec<Vec<Value>>,
}

impl<'a> Persistent<'a> {
    pub fn new(options: &'a Options) -> Result<Self, String> {
        let scenario = crate::lab_validate::load(&options.scenarios[0])?;
        let journey = scenario["id"]
            .as_str()
            .and_then(journey)
            .ok_or("session: unknown journey")?;
        if scenario["budgets"] != journey.budgets() {
            return Err(format!("session: requires budgets {}", journey.budgets()));
        }
        Ok(Self {
            options,
            journey,
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|_| "session: cannot start runtime")?,
            world: world::World::default(),
            launched: false,
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(120),
            traces: vec![],
        })
    }

    async fn step(world: &mut world::World, journey: Journey, step: &Step) -> Result<(), String> {
        let expected = [FIRST, SECOND];
        match step {
            Step::SayText(text) if world.turns < journey.utterances() && text == expected[world.turns] => {
                world.turn().await
            }
            Step::Trace(values) if world.turns == journey.utterances() => {
                world.finish().await?;
                selectors::check(&world.trace.records, &selectors::parse(values)?, Some("run-1"))
            }
            Step::Budget(name, value) => {
                let configured = journey.budgets()[name].as_f64();
                if configured.map(f64::to_bits) == Some(value.to_bits()) {
                    Ok(())
                } else {
                    Err(format!(
                        "session: budget {name} differs from the launched configuration"
                    ))
                }
            }
            _ => Err("session: unsupported step or invalid conversation phase".into()),
        }
    }
}

impl Driver for Persistent<'_> {
    fn execute(&mut self, step: &Step, _: usize, _: Option<&Step>, artifacts: &mut Artifacts) -> Result<(), String> {
        self.runtime.block_on(controlled(
            async {
                if !self.launched {
                    self.launched = true;
                    let bridge = self
                        .options
                        .bridge
                        .as_deref()
                        .ok_or("session: missing production bridge")?;
                    self.world
                        .start(
                            (self.options.python.as_str(), bridge),
                            &artifacts.root.join("session.private"),
                            self.journey,
                            &self.journey.script(),
                        )
                        .await?;
                }
                Self::step(&mut self.world, self.journey, step).await
            },
            self.options.cancel.as_deref(),
            self.deadline,
        ))?;
        if let Step::Trace(values) = step {
            self.traces.push(values.clone());
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
        cleanup?;
        for values in &self.traces {
            selectors::check(&self.world.trace.records, &selectors::parse(values)?, Some("run-1"))?;
        }
        let models = self.world.snapshot["models"].as_array().cloned().unwrap_or_default();
        let observed = self.world.models.clone();
        let summary = oracle::check(
            self.journey,
            &oracle::Evidence {
                records: &self.world.trace.records,
                wire: &self.world.wire,
                models: &observed,
                turns: self.world.turns,
            },
        )?;
        if models.len() != observed.len() {
            return Err("session: provider snapshot and observed model requests differ".into());
        }
        artifacts.event("session.journey", None, Some(&summary.to_string()))?;
        artifacts.checks.insert("session_journey".into(), true);
        Ok(())
    }
}
