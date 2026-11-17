//! Canonical M4: real bridge, pinned app-server, media and hassmic-host.
mod provider;
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
    ui_states: Vec<Value>,
    root: Option<Root>,
    host: Option<Process>,
    bridge: Option<Process>,
    relay: Option<Relay>,
    provider: Option<Provider>,
    spoof: Option<Spoof>,
    trace: Trace,
    speaking: Option<Value>,
    transcript: Option<String>,
    playback: Option<Value>,
}
pub(super) struct Journey {
    pub trace: Trace,
    pub speaking: Option<Value>,
    pub timing: Timing,
    pub completion: Value,
    pub text: String,
    pub ui_states: Vec<Value>,
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
        (Err(primary), Err(cleanup)) => return Err(format!("{primary}; {cleanup}")),
        (Err(error), _) | (_, Err(error)) => return Err(error),
        (Ok(value), Ok(())) => value,
    };
    let text = case.transcript.ok_or("m4: missing observed assistant transcript")?;
    Ok(Journey {
        trace: case.trace,
        speaking: case.speaking,
        timing,
        completion,
        text,
        ui_states: case.ui_states,
        playback: case.playback,
    })
}

impl Case {
    async fn start(&mut self, options: &Options, negative: bool) -> Result<(), String> {
        self.host = Some(Process::start(process::host(&options.python).arg("--finite"))?);
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
        self.provider = Some(Provider::start_mode(&options.python, negative, self.emotion).await?);
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
        self.bridge = Some(Process::start(&mut command)?);
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
        let mut screen = Screen::connect(root).await?;
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
            if emotion {
                observe(&mut screen, states, "emotion").await?;
                observe(&mut screen, states, "connected").await?;
                replies
                    .send(Control::Reply)
                    .await
                    .map_err(|_| "m6: reply gate closed")?;
            }
            let speaking = observe(&mut screen, states, "speaking").await?;
            observe(&mut screen, states, "ready").await?;
            Ok::<_, String>(speaking)
        };
        let (completion, speaking) = tokio::try_join!(settle(bridge, provider, relay, ready), presentation)?;
        self.speaking = Some(speaking);
        let playback = self.host_control(json!({"action":"playback"})).await?;
        if playback != json!({"event":"host.ack","pcm_bytes":9600,"nonzero_bytes":0}) {
            return Err(format!(
                "m4: expected exact decoded host playback PCM, observed {playback}"
            ));
        }
        self.playback = Some(playback);
        let timing = self.relay.as_ref().ok_or("m4: missing relay")?.timing();
        timing.sample()?;
        Ok((completion, timing))
    }

    async fn close(&mut self) -> Result<(), String> {
        let mut errors = vec![];
        if let Some(provider) = &self.provider {
            errors.extend(provider.quiesce().await.err());
        }
        if let Some(mut bridge) = self.bridge.take() {
            errors.extend(bridge.close().await.err());
        }
        let mut snapshot = Value::Null;
        if let Some(provider) = self.provider.take() {
            match provider.close().await {
                Ok(value) => snapshot = value,
                Err(error) => errors.push(error),
            }
        }
        self.transcript = snapshot["transcripts"][1]["text"].as_str().map(str::to_owned);
        if let Some(root) = &self.root {
            let read = std::fs::read(root.0.join("trace")).map_err(|_| "m4: missing final trace".to_owned());
            errors.extend(read.and_then(|bytes| self.trace.append(&bytes)).err());
        }
        self.close_mcp(&snapshot, &mut errors).await;
        if let Some(relay) = self.relay.take() {
            errors.extend(relay.close().await.err());
        }
        if let Some(mut host) = self.host.take() {
            errors.extend(host.close().await.err());
        }
        self.root.take();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
    async fn close_mcp(&mut self, snapshot: &Value, errors: &mut Vec<String>) {
        if let Some(spoof) = self.spoof.take() {
            let checked = spoof
                .snapshot()
                .map_err(|_| "m4: missing MCP observation".to_owned())
                .and_then(|mcp| receipts::merge_mode(&mut self.trace, snapshot, &mcp, self.negative, self.emotion));
            errors.extend(checked.err());
            errors.extend(
                spoof
                    .shutdown()
                    .await
                    .map_err(|_| "m4: spoof shutdown".to_owned())
                    .err(),
            );
        }
    }
}

fn configure(root: &Path) -> Result<(), String> {
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

async fn settle(
    bridge: &mut Process,
    provider: &mut Provider,
    relay: &Relay,
    mut connected: tokio::sync::oneshot::Receiver<()>,
) -> Result<Value, String> {
    let mut admitted = false;
    let mut delegated = false;
    let mut completed = false;
    loop {
        provider.tick().await?;
        if !admitted {
            admitted = match connected.try_recv() {
                Ok(()) => true,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => false,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    return Err("m4: missing connected screen observation".into());
                }
            };
        }
        let timing = relay.timing();
        if admitted && timing.endpoint_seen() && !delegated {
            provider.control(Control::Delegate).await?;
            delegated = true;
        }
        // Eleven CNG packets yield nine frames before converter tail flush.
        if timing.played >= 8640 && !completed {
            if provider.observed.borrow()["appends"] != json!(["delegation-local"]) {
                return Err("m4: reply media before verified native append".into());
            }
            provider.control(Control::Complete).await?;
            completed = true;
        }
        bridge
            .send(&json!({"action":"advance_clock","milliseconds":20}))
            .await?;
        let mut result = None;
        loop {
            let value = bridge.line().await?;
            match value["event"].as_str() {
                Some("bridge.run_completed") => result = Some(value),
                Some("bridge.clock_advanced") if value["milliseconds"] == 20 => break,
                _ => return Err("m4: unexpected bridge clock observation".into()),
            }
        }
        if let Some(result) = result {
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
