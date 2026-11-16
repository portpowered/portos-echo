//! One fresh M4 world. Every acquired child/peer remains owned until close.
use std::path::{Path, PathBuf};

use echo_live::fake::{DialogueLive, DialogueObserver, DialogueScript, FakeLive};
use echo_spoof_mcp::{Fault, Options, Spoof};
use serde_json::{Value, json};

use super::super::{
    process::{self, Process},
    trace::Trace,
};
use super::{
    model::{FALLBACK, Model, SUCCESS},
    screen::Screen,
    wire::{Relay, Timing},
};

#[derive(Default)]
pub struct World {
    pub emotion: bool,
    negative: bool,
    pub ui_states: Vec<Value>,
    pub playback: Option<Value>,
    pub reply_pcm_bytes: usize,
    result_gate: Option<tokio::sync::oneshot::Sender<()>>,
    pub host: Option<Process>,
    pub bridge: Option<Process>,
    relay: Option<Relay>,
    live: Option<DialogueLive>,
    model: Option<Model>,
    spoof: Option<Spoof>,
    root: PathBuf,
    pub trace: Trace,
    pub speaking: Option<Value>,
}

// A finite synthetic 100 ms utterance, distinguishable from startup silence.
// It is sent exactly once through the pinned host's capture backend, not
// inferred from FIFO consumption. The socket observer matches its full sign
// sequence after the host's microphone gain, including cross-frame tails.
pub fn fixture() -> Vec<u8> {
    let mut state = 0x1234_5678_u32;
    (0..1600)
        .flat_map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (if state & 1 == 0 { 1200_i16 } else { -1200_i16 }).to_le_bytes()
        })
        .collect()
}

impl World {
    pub async fn start(&mut self, python: &str, bridge: &Path, root: &Path, negative: bool) -> Result<(), String> {
        std::fs::create_dir(root).map_err(|_| "m4: cannot own private directory")?;
        self.root = root.into();
        self.negative = negative;
        self.host = Some(Process::start(process::host(python).arg("--finite"))?);
        let ready = self.host.as_mut().ok_or("m4: missing host")?.expect("host.ready");
        let address = process::admit_host(&mut vec![], ready, process::readiness_deadline(), process::connect).await?;
        self.relay = Some(Relay::bind_fixture(address, fixture()).await?);
        self.spoof = Some(
            Spoof::start(Options {
                fault: if negative { Fault::Http500 } else { Fault::None },
                ..Options::default()
            })
            .await
            .map_err(|_| "m4: spoof startup failed")?,
        );
        self.start_model(negative).await?;
        self.start_live(negative).await?;
        configure(
            root,
            self.relay.as_ref().ok_or("m4: missing relay")?.address.port(),
            self.spoof.as_ref().ok_or("m4: missing spoof")?,
        )?;
        self.configure_emotion()?;
        let mut command = command(
            root,
            bridge,
            self.live.as_ref().ok_or("m4: missing Live")?.base_url(),
            &self.model.as_ref().ok_or("m4: missing model")?.origin,
        );
        if self.emotion {
            command.arg("--sim-clock");
        }
        self.bridge = Some(Process::start(&mut command)?);
        self.bridge
            .as_mut()
            .ok_or("m4: missing bridge")?
            .expect("bridge.ready")
            .await?;
        self.control(json!({"action":"registered"})).await
    }

    async fn start_model(&mut self, negative: bool) -> Result<(), String> {
        let mut timing = self.relay.as_ref().ok_or("m4: missing relay")?.observations();
        let (release, held) = tokio::sync::oneshot::channel();
        if self.emotion {
            self.result_gate = Some(release);
        }
        self.model = Some(
            Model::bind_controlled(
                negative,
                async move {
                    loop {
                        if timing.borrow().endpoint_seen() {
                            return Ok(());
                        }
                        timing.changed().await.map_err(|_| "m4: capture observer closed")?;
                    }
                },
                self.emotion.then_some(held),
            )
            .await?,
        );
        Ok(())
    }

    async fn start_live(&mut self, negative: bool) -> Result<(), String> {
        let text = if negative {
            FALLBACK
        } else if self.emotion {
            super::emotion::REPLY
        } else {
            SUCCESS
        };
        self.live = Some(
            FakeLive::bind_dialogue(DialogueScript {
                delegation_id: "d-1".into(),
                input: "echo".into(),
                commentary: text.into(),
                transcript: text.into(),
                audio_frames: 10,
                // 14,400 captured samples produce 44 complete packets before
                // the resampler tail is flushed. Wait for actual receipt so
                // audio.done cannot race the final queued capture chunk.
                initial_after_audio_frames: 44,
            })
            .await
            .map_err(|_| "m4: Live startup failed")?,
        );
        Ok(())
    }

    async fn control(&mut self, value: Value) -> Result<(), String> {
        let host = self.host.as_mut().ok_or("m4: missing host")?;
        host.send(&value).await?;
        host.expect("host.ack").await?;
        Ok(())
    }

    pub fn observer(&self) -> Result<DialogueObserver, String> {
        Ok(self.live.as_ref().ok_or("m4: missing Live")?.observer())
    }

    pub async fn speak(&mut self) -> Result<(Value, Timing), String> {
        let mut screen = Screen::connect(&self.root).await?;
        self.observe(&mut screen, 0, "ready").await?;
        self.control(json!({"action":"finite","pcm":fixture()})).await?;
        self.observe(&mut screen, 1, "listening").await?;
        if self.emotion {
            if self.negative {
                self.release_result()?;
            } else {
                self.animate(&mut screen).await?;
            }
        }
        self.speaking = Some(self.observe(&mut screen, 4, "speaking").await?);
        let completion = self
            .bridge
            .as_mut()
            .ok_or("m4: missing bridge")?
            .expect("bridge.run_completed")
            .await?;
        self.observe(&mut screen, 5, "ready").await?;
        let host = self.host.as_mut().ok_or("m4: missing host")?;
        host.send(&json!({"action":"playback"})).await?;
        let playback = host.expect("host.ack").await?;
        let exact_playback = playback == json!({"event":"host.ack","pcm_bytes":9600,"nonzero_bytes":0});
        self.playback = Some(playback);
        let timing = self.relay.as_ref().ok_or("m4: missing relay")?.timing();
        self.reply_pcm_bytes = timing.played;
        if !exact_playback {
            return Err("m4: expected exact decoded host playback PCM".into());
        }
        if self.emotion && timing.played != 9600 {
            return Err(format!("m6: forwarded PCM bytes {}; expected 9600", timing.played));
        }
        timing.sample()?;
        Ok((completion, timing))
    }

    pub fn check_peers(&self) -> Result<(), String> {
        self.model.as_ref().ok_or("m4: missing model")?.check()?;
        let snapshot = self
            .spoof
            .as_ref()
            .ok_or("m4: missing spoof")?
            .snapshot()
            .map_err(|_| "m4: spoof observation failed")?;
        let (name, args) = if self.emotion {
            ("ui.show_emotion", json!({"name":"happy"}))
        } else {
            ("test.echo", json!({"text":"delegated echo"}))
        };
        if snapshot.calls.len() != 1
            || snapshot.calls[0].name != name
            || snapshot.calls[0].args != args
            || snapshot.calls[0].count != 1
        {
            return Err("m4: expected exactly one test.echo attempt with exact arguments".into());
        }
        Ok(())
    }

    pub async fn close(&mut self) -> Result<(), String> {
        let mut failures = Vec::new();
        self.close_bridge(&mut failures).await;
        self.close_peers(&mut failures).await;
        if let Some(mut host) = self.host.take()
            && let Err(error) = host.close().await
        {
            failures.push(error);
        }
        if !self.root.as_os_str().is_empty()
            && let Err(error) = std::fs::remove_dir_all(&self.root)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            failures.push("cleanup: private M4 directory removal failed".into());
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    async fn close_bridge(&mut self, failures: &mut Vec<String>) {
        if let Some(mut bridge) = self.bridge.take() {
            if let Err(error) = bridge.close().await {
                failures.push(error);
            }
            match std::fs::read(self.root.join("trace")) {
                Ok(bytes) => {
                    if let Err(error) = self.trace.append(&bytes).and_then(|()| self.trace.finish()) {
                        failures.push(error);
                    }
                }
                Err(_) => failures.push("m4: missing final bridge trace".into()),
            }
        }
        if self.model.is_some()
            && self.spoof.is_some()
            && let Err(error) = self.check_peers()
        {
            failures.push(error);
        }
    }

    async fn close_peers(&mut self, failures: &mut Vec<String>) {
        self.result_gate.take();
        if let Some(relay) = self.relay.take()
            && let Err(error) = relay.close().await
        {
            failures.push(error);
        }
        if let Some(live) = self.live.take()
            && live.shutdown().await.is_err()
        {
            failures.push("cleanup: Live shutdown failed".into());
        }
        if let Some(model) = self.model.take()
            && let Err(error) = model.close().await
        {
            failures.push(error);
        }
        if let Some(spoof) = self.spoof.take()
            && spoof.shutdown().await.is_err()
        {
            failures.push("cleanup: spoof shutdown failed".into());
        }
    }
}

mod emotion;

pub fn configure(root: &Path, port: u16, spoof: &Spoof) -> Result<(), String> {
    let config = format!(
        r#"
[echo]
host = "127.0.0.1"
wyoming_port = {port}
[live]
mode = "api_key"
credential_file = "credential"
voice = "alloy"
session_idle_seconds = 120
[agent]
model = "fixture-model"
max_tool_calls = 8
task_deadline_seconds = 10
daily_request_cap = 10
[[mcp.server]]
name = "synthetic"
url = "{}"
auth = "none"
allow_tools = ["test.echo"]
[screen]
listen = "127.0.0.1:0"
pairing_token_file = "pairing"
[audit]
log_dir = "audit"
[chatgpt_auth]
token_endpoint = "http://127.0.0.1:1/token"
"#,
        spoof.url()
    );
    for (name, value) in [
        ("bridge.toml", config.as_str()),
        ("credential", "synthetic-m4-key"),
        ("pairing", "test-pair"),
    ] {
        std::fs::write(root.join(name), value).map_err(|_| "m4: cannot write synthetic configuration")?;
    }
    Ok(())
}

pub fn command(root: &Path, bridge: &Path, live: &str, model: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(bridge);
    command.current_dir(root).args([
        "--config",
        "bridge.toml",
        "--trace",
        "trace",
        "--health-listen",
        "127.0.0.1:0",
        "--live-loopback",
        live,
        "--model-loopback",
        model,
    ]);
    for (key, _) in std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("ECHO_LAB_")) {
        command.env_remove(key);
    }
    command
}

#[cfg(test)]
mod lifecycle;
#[cfg(test)]
mod tests;
