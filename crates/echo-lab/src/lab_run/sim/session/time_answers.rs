//! Direct Live replies use the received startup and retained-wake time context.
mod clock;
mod oracle;
mod refresh;

use super::{duplex::World, provider::Control};
use crate::lab_run::{Driver, Step, run_options::Options, run_report::Artifacts, sim::controlled};
use serde_json::{Value, json};
use std::{net::SocketAddr, path::Path, time::Duration};
use tokio::time::Instant;

pub const ID: &str = "session-live-time-answers";
pub const REFRESH_ID: &str = "session-live-time-refresh";
const REPEATED: [&str; 2] = ["What time is it?"; 2];
const QUESTIONS: [&str; 4] = [
    "What time is it?",
    "What is today's date?",
    "What day of the week is it?",
    "What time is it in my time zone?",
];
const CONFIGURED_TURNS: usize = 3;

pub fn admits(id: &str) -> bool {
    matches!(id, ID | REFRESH_ID)
}

pub fn utterance(value: &str) -> Result<Step, String> {
    if !QUESTIONS.contains(&value) {
        return Err("time answers: unexpected synthetic utterance".into());
    }
    Ok(Step::SayText(value.into()))
}

pub struct TimeAnswers<'a> {
    options: &'a Options,
    runtime: tokio::runtime::Runtime,
    deadline: Instant,
    world: Option<TimeWorld>,
    turns: usize,
    reached_budget: bool,
    refresh: bool,
}

impl<'a> TimeAnswers<'a> {
    pub fn new(options: &'a Options) -> Result<Self, String> {
        let scenario = crate::lab_validate::load(&options.scenarios[0])?;
        if scenario["budgets"] != json!({"session_listen_seconds":3}) {
            return Err("time answers: expected the configured 3-second listen window".into());
        }
        Ok(Self {
            options,
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|_| "time answers: runtime unavailable")?,
            deadline: Instant::now() + Duration::from_secs(120),
            world: None,
            turns: 0,
            reached_budget: false,
            refresh: scenario["id"] == REFRESH_ID,
        })
    }

    fn questions(&self) -> &'static [&'static str] {
        if self.refresh { &REPEATED } else { &QUESTIONS }
    }
}

impl Driver for TimeAnswers<'_> {
    fn execute(&mut self, step: &Step, _: usize, _: Option<&Step>, artifacts: &mut Artifacts) -> Result<(), String> {
        match step {
            Step::Budget(name, value) if name == "session_listen_seconds" && value.to_bits() == 3.0_f64.to_bits() => {
                self.reached_budget = true;
                return Ok(());
            }
            Step::SayText(text) if self.questions().get(self.turns) == Some(&text.as_str()) => {}
            _ => return Err("time answers: unexpected scenario step".into()),
        }
        self.runtime.block_on(controlled(
            async {
                if self.world.is_none() {
                    let configured = self.turns == 0;
                    self.world = Some(TimeWorld::start(self.options, &artifacts.root, configured, self.refresh).await?);
                }
                let world = self.world.as_mut().ok_or("time answers: world unavailable")?;
                world.turn(self.turns, artifacts).await?;
                self.turns += 1;
                if self.turns == CONFIGURED_TURNS {
                    self.world
                        .take()
                        .ok_or("time answers: configured world unavailable")?
                        .close(artifacts)
                        .await?;
                }
                Ok(())
            },
            self.options.cancel.as_deref(),
            self.deadline,
        ))
    }

    fn close(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
        if let Some(world) = self.world.take() {
            self.runtime.block_on(world.close(artifacts))?;
        }
        let questions = self.questions();
        let complete = self.turns == questions.len();
        let check = if self.refresh {
            "live_time_refresh"
        } else {
            "live_time_answers"
        };
        artifacts.checks.insert(check.into(), complete);
        if self.reached_budget && !complete {
            return Err(format!(
                "time answers: completed {} of {} questions; missing {:?}",
                self.turns,
                questions.len(),
                questions[self.turns],
            ));
        }
        // Dispatch already reports the original error if a turn failed before the budget.
        Ok(())
    }
}

struct TimeWorld {
    world: World,
    clock: clock::Clock,
    configured: bool,
    refresh: bool,
    turns: usize,
    context: Option<Value>,
}

impl TimeWorld {
    async fn start(options: &Options, output: &Path, configured: bool, refresh: bool) -> Result<Self, String> {
        let (clock, address) = clock::Clock::start().await?;
        let private = if configured {
            "time-configured.private"
        } else {
            "time-utc.private"
        };
        // If the Live voice hands the question off, the original provider wire
        // path accepts the runner request. It supplies no direct voice reply.
        let script = json!([[{"say":"The runner took the time question."}]]);
        let world = World::start(options, output.join(private), &script, |root| {
            configure(root, address, configured, refresh)
        })
        .await?;
        Ok(Self {
            world,
            clock,
            configured,
            refresh,
            turns: 0,
            context: None,
        })
    }

    async fn turn(&mut self, question: usize, artifacts: &mut Artifacts) -> Result<(), String> {
        let world = &mut self.world;
        if self.refresh {
            refresh::ask(world, &self.clock, self.turns).await?;
        } else {
            world.satellite.speak(self.turns == 0).await?;
            if self.turns == 0 {
                world.connected().await?;
            }
            world
                .provider
                .control(Control::TimeQuestion(QUESTIONS[question]))
                .await?;
        }
        world
            .wait(
                "provider time decision",
                |world| world.provider.count("time_turns") > self.turns,
                0,
            )
            .await?;
        let result = world.provider.observed.borrow()["time_turns"][self.turns].clone();
        if result["answer"].is_null() {
            world
                .wait(
                    "delegation.created from the pinned app server",
                    |world| world.count("delegation.created") > 0,
                    0,
                )
                .await?;
            return oracle::delegated(world, artifacts, &result);
        }
        let answer = if self.refresh {
            artifacts.event("session.live_time_decision", Some(question), Some(&result.to_string()))?;
            refresh::answer(&result, question)?
        } else {
            let context = self.context.get_or_insert_with(|| result["context"].clone());
            if *context != result["context"] {
                return Err("time answers: retained run's context changed".into());
            }
            oracle::answer(&result, question)?
        };
        let start_bytes = world.start_pcm(self.turns).await?;
        world.tail_pcm(start_bytes).await?;
        world.played(self.turns, answer).await?;
        let reply = oracle::reply(world, &result, self.turns)?;
        artifacts.event("session.live_time_answer", Some(question), Some(&reply.to_string()))?;
        self.turns += 1;
        Ok(())
    }

    async fn close(self, artifacts: &mut Artifacts) -> Result<(), String> {
        let (snapshot, trace) = self.world.close().await?;
        let evidence = oracle::counts(&snapshot, &trace.records, self.configured);
        artifacts.event("session.live_time_counts", None, Some(&evidence.to_string()))?;
        if self.refresh {
            return refresh::check(&snapshot, &trace.records, &evidence, self.turns, artifacts);
        }
        let expected = if self.configured { CONFIGURED_TURNS } else { 1 };
        if self.turns == expected {
            oracle::direct_only(&evidence, expected, 1)?;
        }
        Ok(())
    }
}

fn configure(root: &Path, address: SocketAddr, configured: bool, refresh: bool) -> Result<(), String> {
    let path = root.join("configured.private/bridge.toml");
    let mut text = std::fs::read_to_string(&path).map_err(|_| "time answers: missing configuration")?;
    if configured {
        let home = root.join("agent-home");
        let prompts = home.join("prompts");
        std::fs::create_dir_all(&prompts).map_err(|_| "time answers: cannot create agent home")?;
        std::fs::write(
            prompts.join("voice.md"),
            include_str!("../../../../../../examples/prompts/voice.md"),
        )
        .map_err(|_| "time answers: cannot seed example voice prompt")?;
        text = format!(
            "{text}time_zone = 'America/Los_Angeles'\n\n[home]\ndir = '{}'\nagent_tools = false\n",
            home.display()
        );
    }
    let interval = if refresh { "interval_seconds = 60\n" } else { "" };
    std::fs::write(path, format!("{text}\n[clock]\nservers = ['{address}']\n{interval}"))
        .map_err(|_| "time answers: cannot write private configuration".into())
}
