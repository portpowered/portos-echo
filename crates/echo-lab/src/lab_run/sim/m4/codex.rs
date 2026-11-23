//! Canonical M4: real bridge, pinned app-server, media and hassmic-host.
#[cfg(test)]
#[path = "world/emotion.rs"]
mod emotion_tests;
#[cfg(test)]
#[path = "world/tests.rs"]
mod host_tests;
#[cfg(test)]
#[path = "world/lifecycle.rs"]
mod lifecycle;
pub(in crate::lab_run::sim) mod provider;
mod receipts;
#[cfg(test)]
mod tests;
use super::{
    screen::Screen,
    wire::{Relay, Timing},
    world,
};
use crate::lab_run::{
    run_options::Options,
    sim::{
        controlled,
        m2::codex::config::Root,
        process::{self, Process},
        trace::Trace,
    },
};
use echo_spoof_mcp::{Fault, Options as SpoofOptions, Spoof};
use provider::{Control, Provider};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

#[derive(Default)]
struct Case {
    negative: bool,
    emotion: bool,
    hold_model: bool,
    ui_states: Vec<Value>,
    mouth: Vec<Value>,
    screen: Option<Screen>,
    root: Option<Root>,
    host: Option<Process>,
    bridge: Option<Process>,
    relay: Option<Relay>,
    provider: Option<Provider>,
    spoof: Option<Spoof>,
    trace: Trace,
    speaking: Option<Value>,
    transcript: Option<String>,
    snapshot: Value,
    bridge_exit: Option<String>,
    playback: Option<Value>,
    clock: Value,
    #[cfg(test)]
    released: Vec<&'static str>,
}
pub(super) struct Journey {
    pub trace: Trace,
    pub speaking: Option<Value>,
    pub timing: Timing,
    pub completion: Value,
    pub text: String,
    pub ui_states: Vec<Value>,
    pub mouth: Vec<Value>,
    pub playback: Option<Value>,
}

pub(super) async fn run(options: &Options, negative: bool) -> Result<Journey, String> {
    let mut case = Case {
        emotion: crate::lab_validate::load(&options.scenarios[0])?["id"] == "m6-emotion",
        ..Case::default()
    };
    let work = controlled(
        Box::pin(async {
            case.start(options, negative).await?;
            case.speak().await
        }),
        options.cancel.as_deref(),
        tokio::time::Instant::now() + Duration::from_secs(30),
    )
    .await;
    let close = case.close().await;
    let (completion, timing) = match (work, close) {
        (Err(primary), Err(cleanup)) => return Err(case.failure(&format!("{primary}; {cleanup}"))),
        (Err(error), _) | (_, Err(error)) => return Err(case.failure(&error)),
        (Ok(value), Ok(())) => value,
    };
    let text = case.transcript.ok_or("m4: missing observed assistant transcript")?;
    receipts::screen(&case.ui_states, &case.clock)?;
    Ok(Journey {
        trace: case.trace,
        speaking: case.speaking,
        timing,
        completion,
        text,
        ui_states: case.ui_states,
        mouth: case.mouth,
        playback: case.playback,
    })
}

impl Case {
    // These are local synthetic-fixture observations, never credential files,
    // raw process stderr or arbitrary model requests. Keep them after cleanup
    // so a failed CLI journey retains its primary evidence in the caller's log.
    fn failure(&self, error: &str) -> String {
        let observations = json!({
            "bridge_exit":self.bridge_exit,
            "bridge_trace":self.trace.records,
            "provider_expected":receipts::expected(self.negative, self.emotion),
            "provider":self.snapshot,
            "playout_clock":self.clock,
        });
        format!("{error}; m4 failure observations: {observations}")
    }

    async fn start(&mut self, options: &Options, negative: bool) -> Result<(), String> {
        self.start_host(options, negative, &options.python).await
    }

    async fn start_host(&mut self, options: &Options, negative: bool, python: &str) -> Result<(), String> {
        self.host = Some(Process::start(process::host(python).arg("--finite"))?);
        let ready = self.host.as_mut().ok_or("m4: missing host")?.expect("host.ready");
        let address = process::admit_host(&mut vec![], ready, process::readiness_deadline(), process::connect).await?;
        self.connect(options, negative, address).await?;
        self.host_control(json!({"action":"registered"})).await.map(|_| ())
    }

    async fn connect(
        &mut self,
        options: &Options,
        negative: bool,
        address: std::net::SocketAddr,
    ) -> Result<(), String> {
        self.negative = negative;
        self.root = Some(Root::new()?);
        self.relay = Some(Relay::bind_fixture(address, world::fixture()).await?);
        self.spoof = Some(
            Spoof::start(SpoofOptions {
                fault: Fault::None,
                ..SpoofOptions::default()
            })
            .await
            .map_err(|_| "m4: spoof startup")?,
        );
        self.provider = Some(Provider::start_mode(&options.python, negative, self.emotion, self.hold_model).await?);
        let root = &self.root.as_ref().ok_or("m4: missing root")?.0;
        world::configure(
            root,
            self.relay.as_ref().ok_or("m4: missing relay")?.address.port(),
            self.spoof.as_ref().ok_or("m4: missing spoof")?,
        )?;
        configure(root)?;
        let origin = &self.provider.as_ref().ok_or("m4: missing provider")?.origin;
        let mut command = world::command(
            root,
            options.bridge.as_deref().ok_or("m4: missing bridge")?,
            origin,
            origin,
        );
        command.arg("--sim-clock");
        self.bridge = Some(Process::start_bridge(&mut command, self.hold_model)?);
        self.bridge
            .as_mut()
            .ok_or("m4: missing bridge")?
            .expect("bridge.ready")
            .await?;
        Ok(())
    }

    async fn host_control(&mut self, value: Value) -> Result<Value, String> {
        let host = self.host.as_mut().ok_or("m4: missing host")?;
        host.send(&value).await?;
        host.expect("host.ack").await
    }

    async fn speak(&mut self) -> Result<(Value, Timing), String> {
        let root = &self.root.as_ref().ok_or("m4: missing root")?.0;
        let mut screen = Screen::lips(root).await?;
        observe(&mut screen, &mut self.ui_states, "ready").await?;
        self.host_control(json!({"action":"finite","pcm":world::fixture()}))
            .await?;
        let provider = self.provider.as_mut().ok_or("m4: missing provider")?;
        let bridge = self.bridge.as_mut().ok_or("m4: missing bridge")?;
        let relay = self.relay.as_ref().ok_or("m4: missing relay")?;
        let (connected, ready) = tokio::sync::oneshot::channel();
        let spoof = self.spoof.as_ref().ok_or("m4: missing spoof")?;
        let negative = self.negative;
        let emotion = self.emotion;
        let replies = provider.commands.clone();
        let states = &mut self.ui_states;
        let presentation = async {
            observe(&mut screen, states, "listening").await?;
            observe(&mut screen, states, "connected").await?;
            if negative {
                spoof
                    .set_fault(Fault::Http500)
                    .map_err(|_| "m4: cannot inject MCP fault")?;
            }
            connected.send(()).map_err(|()| "m4: connection observer abandoned")?;
            observe(&mut screen, states, "thinking").await?;
            if emotion {
                if !negative {
                    observe(&mut screen, states, "emotion").await?;
                    observe(&mut screen, states, "thinking").await?;
                }
                replies
                    .send(Control::Reply)
                    .await
                    .map_err(|_| "m6: reply gate closed")?;
            }
            let speaking = observe(&mut screen, states, "speaking").await?;
            observe(&mut screen, states, "connected").await?;
            observe(&mut screen, states, "ready").await?;
            Ok::<_, String>(speaking)
        };
        let (completion, speaking) =
            tokio::try_join!(settle(bridge, provider, relay, ready, &mut self.clock), presentation)?;
        self.speaking = Some(speaking);
        self.screen = Some(screen);
        let playback = self.host_control(json!({"action":"playback"})).await?;
        let timing = self.relay.as_ref().ok_or("m4: missing relay")?.timing();
        let samples = timing.audible_reply(&playback)?;
        let mut playback = playback;
        playback["nonzero_samples"] = samples["nonzero_samples"].clone();
        playback["audible_samples"] = samples["audible_samples"].clone();
        playback["playout_clock"] = self.clock.clone();
        self.playback = Some(playback);
        timing.sample()?;
        Ok((completion, timing))
    }

    async fn close(&mut self) -> Result<(), String> {
        let mut errors = vec![];
        if let Some(provider) = &self.provider {
            errors.extend(provider.quiesce().await.err());
        }
        self.close_bridge(&mut errors).await;
        errors.extend(self.collect_screen().await.err());
        let mut snapshot = Value::Null;
        if let Some(provider) = self.provider.take() {
            match provider.close().await {
                Ok(value) => {
                    #[cfg(test)]
                    self.released.push("provider");
                    snapshot = value;
                }
                Err(error) => errors.push(error),
            }
        }
        self.transcript = snapshot["transcripts"][1]["text"].as_str().map(str::to_owned);
        self.close_mcp(&snapshot, &mut errors).await;
        if let Some(relay) = self.relay.take() {
            let closed = relay.close().await;
            #[cfg(test)]
            self.record_release("relay", &closed);
            errors.extend(closed.err());
        }
        if let Some(mut host) = self.host.take() {
            let closed = host.close().await;
            #[cfg(test)]
            self.record_release("host", &closed);
            errors.extend(closed.err());
        }
        self.snapshot = snapshot;
        errors.extend(self.remove_root().err());
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
    async fn collect_screen(&mut self) -> Result<(), String> {
        if let Some(mut screen) = self.screen.take() {
            self.ui_states.extend(screen.remaining().await?);
            self.mouth = screen.mouth;
        }
        Ok(())
    }
    async fn close_bridge(&mut self, errors: &mut Vec<String>) {
        if let Some(mut bridge) = self.bridge.take() {
            let closed = bridge.close().await;
            #[cfg(test)]
            self.record_release("bridge", &closed);
            errors.extend(closed.err());
            self.bridge_exit = Some(bridge.exit_summary());
            if let Some(root) = &self.root {
                let read = std::fs::read(root.0.join("trace")).map_err(|_| "m4: missing final trace".to_owned());
                errors.extend(read.and_then(|bytes| self.trace.append(&bytes)).err());
            }
        }
    }
    fn remove_root(&mut self) -> Result<(), String> {
        self.root.take().map_or(Ok(()), |root| remove_root(&root.0))
    }
    async fn close_mcp(&mut self, snapshot: &Value, errors: &mut Vec<String>) {
        if let Some(spoof) = self.spoof.take() {
            let checked = spoof
                .snapshot()
                .map_err(|_| "m4: missing MCP observation".to_owned())
                .and_then(|mcp| receipts::merge_mode(&mut self.trace, snapshot, &mcp, self.negative, self.emotion));
            errors.extend(checked.err());
            let closed = spoof.shutdown().await;
            #[cfg(test)]
            self.record_release("spoof", &closed);
            errors.extend(closed.err().map(|_| "m4: spoof shutdown".to_owned()));
        }
    }
}

pub(in crate::lab_run::sim) fn configure(root: &Path) -> Result<(), String> {
    let path = root.join("bridge.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|_| "m4: missing config")?
        .replace("voice = \"alloy\"\n", "")
        .replace("model = \"fixture-model\"", "model = \"gpt-5.4\"")
        .replace(
            "allow_tools = [\"test.echo\"]",
            "allow_tools = [\"test.echo\", \"test.wait\", \"test.requires_approval\", \"ui.show_emotion\"]",
        );
    std::fs::write(
        path,
        format!("{text}\n[codex]\nhome = '{}'\n", root.join("home").display()),
    )
    .map_err(|_| "m4: cannot configure Codex".into())
}

pub(in crate::lab_run::sim) async fn settle(
    bridge: &mut Process,
    provider: &mut Provider,
    relay: &Relay,
    mut connected: tokio::sync::oneshot::Receiver<()>,
    clock: &mut Value,
) -> Result<Value, String> {
    let mut admitted = false;
    let mut delegated = false;
    let mut completed = false;
    let mut tail = false;
    let mut released = false;
    let mut idle = false;
    loop {
        // RTP and transcript gates follow the media cadence. Once the whole
        // reply is released, received and Played, only the bridge's listen clock remains.
        if !idle {
            provider.tick().await?;
        }
        if !admitted {
            admitted = match connected.try_recv() {
                Ok(()) => true,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => false,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    return Err("m4: missing connected screen observation".into());
                }
            };
        }
        if !released && provider.observed.borrow()["reply_ready"] == true {
            // This owner freezes time before opening the provider's RTP gate.
            released = true;
            provider.control(Control::Release).await?;
        }
        let timing = relay.timing();
        if admitted && timing.endpoint_seen() && !delegated {
            provider.control(Control::Delegate).await?;
            delegated = true;
        }
        if timing.played >= 2880 && !tail {
            provider.control(Control::Tail).await?;
            tail = true;
        }
        // Eleven synthetic tone packets yield nine frames before converter tail flush.
        if timing.played >= 8640 && !completed {
            if provider.observed.borrow()["appends"] != json!(["delegation-local"]) {
                return Err("m4: reply media before verified native append".into());
            }
            provider.control(Control::Complete).await?;
            completed = true;
        }
        let milliseconds = if released && timing.played < 9600 { 0 } else { 20 };
        idle = completed && timing.played >= 9600 && timing.replies_played() == 1;
        let result = crate::lab_run::sim::m2::codex::collect_playout(bridge, clock, milliseconds).await?;
        if let Some(result) = result {
            relay.wait_session(1, 1).await?;
            return Ok(result);
        }
    }
}

async fn observe(screen: &mut Screen, states: &mut Vec<Value>, name: &str) -> Result<Value, String> {
    let frame = screen.next().await?;
    states.push(frame.clone());
    if frame["state"]["state"] != name {
        return Err(format!("screen: expected {name}, observed {}", frame["state"]));
    }
    Ok(frame["state"].clone())
}

fn remove_root(root: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("cleanup: private M4 directory removal failed".into()),
    }
}
