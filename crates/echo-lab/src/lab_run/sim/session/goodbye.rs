//! Live-originated goodbye through the shipped bridge and pinned app server.
mod home;
use super::{duplex::World, provider::Control};
use crate::lab_run::{Driver, Step, run_options::Options, run_report::Artifacts, sim::controlled};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::Instant;

pub(super) const GOODBYE: &str = "okay thanks, bye";
const QUESTION: &str = "thanks, and what time is it?";
const FOLLOWUP: &str = "what day is it?";
const FAREWELL: &str = "Goodbye.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Journey {
    Goodbye,
    /// The same goodbye with the runner the Echo really uses: a code-mode-only
    /// model, which reaches the bridge's tools only as direct declarations.
    Native,
    Pending,
    Question,
}

impl Journey {
    fn from_id(id: &str) -> Option<Self> {
        match id {
            "session-live-goodbye" => Some(Self::Goodbye),
            "session-live-goodbye-native" => Some(Self::Native),
            "session-live-goodbye-pending" => Some(Self::Pending),
            "session-live-thanks-question" => Some(Self::Question),
            _ => None,
        }
    }

    fn utterances(self) -> [&'static str; 2] {
        match self {
            Self::Goodbye | Self::Native => [GOODBYE, GOODBYE],
            Self::Pending => ["what time is it?", GOODBYE],
            Self::Question => [QUESTION, FOLLOWUP],
        }
    }

    fn script(self) -> Value {
        // The hand-off is the real wire shape: the user's own words. The
        // scripted runner stands in for the model that reads them.
        let end = |wait_for_release| {
            json!([
                {"request":GOODBYE,"model":self.model(),"call":"end_session","arguments":{},
                 "wait_for_release":wait_for_release},
                {"silent":true}
            ])
        };
        match self {
            Self::Goodbye | Self::Native => json!([end(false), end(true)]),
            Self::Pending => json!([[{"say":"It is noon.","wait_for_release":true}], end(false)]),
            Self::Question => json!([]),
        }
    }

    fn model(self) -> &'static str {
        if self == Self::Native {
            "gpt-6.1-sol"
        } else {
            home::MODEL
        }
    }
}

pub fn admits(id: &str) -> bool {
    Journey::from_id(id).is_some()
}

pub fn utterance(id: &str, text: &str) -> Result<Step, String> {
    if Journey::from_id(id).is_some_and(|journey| journey.utterances().contains(&text)) {
        Ok(Step::SayText(text.into()))
    } else {
        Err("goodbye: unexpected synthetic utterance".into())
    }
}

pub struct Goodbye<'a> {
    options: &'a Options,
    runtime: tokio::runtime::Runtime,
    journey: Journey,
    deadline: Instant,
    world: Option<World>,
    turns: usize,
    observations: Vec<Value>,
}

impl<'a> Goodbye<'a> {
    pub fn new(options: &'a Options) -> Result<Self, String> {
        let scenario = crate::lab_validate::load(&options.scenarios[0])?;
        let journey = scenario["id"]
            .as_str()
            .and_then(Journey::from_id)
            .ok_or("goodbye: unknown journey")?;
        if scenario["budgets"] != json!({"session_listen_seconds":3}) {
            return Err("goodbye: expected the configured 3-second listen window".into());
        }
        Ok(Self {
            options,
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|_| "goodbye: runtime unavailable")?,
            journey,
            deadline: Instant::now() + Duration::from_secs(120),
            world: None,
            turns: 0,
            observations: vec![],
        })
    }
}

impl Driver for Goodbye<'_> {
    fn execute(&mut self, step: &Step, _: usize, _: Option<&Step>, artifacts: &mut Artifacts) -> Result<(), String> {
        self.runtime.block_on(controlled(
            async {
                let journey = self.journey;
                match step {
                    Step::SayText(text) if journey.utterances().get(self.turns) == Some(&text.as_str()) => {
                        if self.world.is_none() {
                            self.world = Some(
                                World::start(
                                    self.options,
                                    artifacts.root.join("goodbye.private"),
                                    &journey.script(),
                                    |root| home::installed(root, journey.model()),
                                )
                                .await?,
                            );
                        }
                        let world = self.world.as_mut().ok_or("goodbye: missing started world")?;
                        if let Some(result) = world.turn(journey, self.turns, text).await? {
                            self.observations.push(result);
                        }
                        self.turns += 1;
                        Ok(())
                    }
                    Step::Budget(name, value)
                        if name == "session_listen_seconds" && value.to_bits() == 3.0_f64.to_bits() =>
                    {
                        Ok(())
                    }
                    _ => Err("goodbye: unexpected scenario step".into()),
                }
            },
            self.options.cancel.as_deref(),
            self.deadline,
        ))
    }

    fn close(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
        let world = self.world.take().ok_or("goodbye: world was not started")?;
        let (snapshot, _) = self.runtime.block_on(world.close())?;
        if self.turns != 2 || snapshot["calls"] != 1 || snapshot["sidebands"] != 1 || snapshot["external"] != 0 {
            return Err("goodbye: expected two scripted turns through one local Live call".into());
        }
        for evidence in self.observations.drain(..) {
            let event = evidence["event"].as_str().ok_or("goodbye: missing evidence event")?;
            artifacts.event(event, None, Some(&evidence.to_string()))?;
        }
        artifacts.checks.insert("live_goodbye_journey".into(), true);
        Ok(())
    }
}

impl World {
    async fn turn(&mut self, journey: Journey, index: usize, text: &str) -> Result<Option<Value>, String> {
        match journey {
            Journey::Goodbye | Journey::Native => self.wake_and_farewell(index, text).await.map(Some),
            Journey::Pending => self.pending(index, text).await,
            Journey::Question => self.question(index, text).await,
        }
    }

    async fn wake_and_farewell(&mut self, index: usize, text: &str) -> Result<Value, String> {
        if index == 1 {
            self.run_id = "run-2";
        }
        self.satellite.speak(true).await?;
        let endpoint = self.epoch();
        if index == 0 {
            self.connected().await?;
        }
        self.transcript("goodbye", "user", text, true).await?;
        self.farewell(index, index == 1, endpoint).await
    }

    async fn pending(&mut self, index: usize, text: &str) -> Result<Option<Value>, String> {
        if index == 0 {
            self.satellite.speak(true).await?;
            self.connected().await?;
            self.provider.control(Control::Delegate(0)).await?;
            self.wait("prior runner request", |world| world.provider.count("waiting") == 1, 20)
                .await?;
            return Ok(None);
        }
        self.satellite.speak(false).await?;
        let endpoint = self.epoch();
        self.transcript("pending-goodbye", "user", text, true).await?;
        self.provider.control(Control::ReleaseGoodbye).await?;
        self.wait(
            "prior handback",
            |world| world.provider.count("appends") == 1 && world.count("delegation.resolved") == 1,
            20,
        )
        .await?;
        self.reply("It is noon.", 0, None).await?;
        self.farewell(1, false, endpoint).await.map(Some)
    }

    async fn farewell(&mut self, turn: usize, late: bool, endpoint: u64) -> Result<Value, String> {
        let reply = self.satellite.observed.borrow().replies_played();
        self.reply(FAREWELL, reply, Some((turn, late))).await?;
        if late {
            if self.count("session.tool_receipt") != 0 || self.count("wyoming.run_ended") != 0 {
                return Err("goodbye: end or receipt before the held runner was released".into());
            }
            // Advance the guarded clock so the hand-off is observably later than Played.
            self.step(20).await?;
            self.provider.control(Control::Delegate(turn)).await?;
            self.wait("late native goodbye", |world| world.provider.count("waiting") == 1, 0)
                .await?;
            // A follow-up spoken in the gap before the runner's end arrives.
            self.transcript("canary", "user", FOLLOWUP, true).await?;
            self.wait(
                "canary delivery",
                |world| {
                    world.provider.observed.borrow()["events_sent"]
                        .as_array()
                        .is_some_and(|events| events.contains(&json!("canary")))
                },
                20,
            )
            .await?;
            self.step(20).await?;
            self.provider.control(Control::ReleaseGoodbye).await?;
        }
        self.wait("explicit run close", |world| world.count("wyoming.run_ended") == 1, 20)
            .await?;
        self.wait(
            "runner tool response",
            |world| {
                world.provider.observed.borrow()["models"]
                    .as_array()
                    .is_some_and(|models| {
                        models
                            .iter()
                            .any(|model| model["turn"] == turn && model["outputs"] != json!([]))
                    })
                    && world.count("delegation.resolved") >= if reply == 1 && world.run_id == "run-1" { 2 } else { 1 }
            },
            20,
        )
        .await?;
        self.ready().await?;
        super::goodbye_oracle::farewell(
            &self.trace.records,
            &self.satellite.observed.borrow(),
            &self.provider.observed.borrow(),
            (self.run_id, turn, reply, late),
            endpoint,
            &self.clock,
        )
    }

    async fn question(&mut self, index: usize, text: &str) -> Result<Option<Value>, String> {
        self.satellite.speak(index == 0).await?;
        if index == 0 {
            self.connected().await?;
        }
        self.transcript("question", "user", text, true).await?;
        self.reply(if index == 0 { "It is noon." } else { "It is Tuesday." }, index, None)
            .await?;
        if self.count("wyoming.run_ended") != 0
            || self.count("session.tool_receipt") != 0
            || self.provider.count("delegations") != 0
        {
            return Err("goodbye: a question unexpectedly ended or delegated the run".into());
        }
        if index == 0 {
            return Ok(None);
        }
        self.step(3_000).await?;
        self.wait("ordinary idle close", |world| world.count("wyoming.run_ended") == 1, 20)
            .await?;
        self.ready().await?;
        super::goodbye_oracle::question(&self.trace.records, &self.satellite.observed.borrow(), &self.clock).map(Some)
    }

    async fn reply(&mut self, text: &'static str, index: usize, goodbye: Option<(usize, bool)>) -> Result<(), String> {
        self.transcript("reply-delta", "assistant", text, false).await?;
        let start_bytes = self.start_pcm(index).await?;
        if let Some((turn, false)) = goodbye {
            self.provider.control(Control::Delegate(turn)).await?;
            self.wait(
                "native goodbye",
                |world| world.count("delegation.created") >= if turn == 1 && world.run_id == "run-1" { 2 } else { 1 },
                0,
            )
            .await?;
        }
        self.tail_pcm(start_bytes).await?;
        if let Some((_, false)) = goodbye {
            self.wait(
                "early session receipt",
                |world| world.count("session.tool_receipt") == 1,
                0,
            )
            .await?;
        }
        self.played(index, text).await
    }
}
