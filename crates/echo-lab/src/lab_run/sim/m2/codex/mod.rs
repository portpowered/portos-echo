//! Pinned-server M2 success path; the provider and its media endpoint are local.
pub(in crate::lab_run::sim) mod config;
#[cfg(test)]
mod lifecycle;
pub(in crate::lab_run::sim) mod provider;
pub(super) mod rejection;
#[cfg(test)]
mod tests;
pub(in crate::lab_run::sim) mod tone;

use super::{DEADLINE, oracle};
use crate::lab_run::{
    run_options::Options,
    sim::{
        controlled,
        process::{self, Process},
        production::Screen,
        trace::Trace,
    },
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct Case {
    host: Option<Process>,
    relay: Option<crate::lab_run::sim::m4::wire::Relay>,
    playback: Value,
    bridge: Option<Process>,
    provider: Option<provider::Provider>,
    collector: Option<tokio::task::JoinHandle<Result<(), String>>>,
    frames: Arc<Mutex<Vec<Value>>>,
    root: Option<config::Root>,
    trace: Trace,
    trace_path: PathBuf,
}

impl Case {
    async fn start(&mut self, options: &Options) -> Result<(), String> {
        let mut command = process::host(&options.python);
        command.arg("--finite");
        self.host = Some(Process::start(&mut command)?);
        let host = self.host.as_mut().ok_or("m2: missing host")?;
        let address = process::admit_host(
            &mut vec![],
            host_ready(host),
            process::readiness_deadline(),
            process::connect,
        )
        .await?;
        self.start_bridge(options, address.port()).await?;
        let host = self.host.as_mut().ok_or("m2: missing host")?;
        host.send(&json!({"action":"registered"})).await?;
        host.expect("host.ack").await?;
        Ok(())
    }

    async fn start_bridge(&mut self, options: &Options, port: u16) -> Result<(), String> {
        self.root = Some(config::Root::new()?);
        let root = self.root.as_ref().ok_or("m2: missing private root")?;
        if self.trace_path.as_os_str().is_empty() {
            self.trace_path = root.0.join("trace");
        }
        self.provider = Some(provider::Provider::start(&options.python).await?);
        let origin = &self.provider.as_ref().ok_or("m2: missing provider")?.origin;
        let mut command = tokio::process::Command::new(options.bridge.as_deref().ok_or("m2: missing bridge")?);
        let relay = crate::lab_run::sim::m4::wire::Relay::bind_reply(([127, 0, 0, 1], port).into()).await?;
        let port = relay.address.port();
        self.relay = Some(relay);
        config::configure(&mut command, &root.0, port, origin, &self.trace_path)?;
        self.bridge = Some(Process::start(&mut command)?);
        self.bridge
            .as_mut()
            .ok_or("m2: missing bridge")?
            .expect("bridge.ready")
            .await?;
        self.trace = read_trace(&self.trace_path)?;
        let address = self
            .trace
            .records
            .iter()
            .find(|r| r["event"] == "screen.listening")
            .and_then(|r| r["address"].as_str())
            .ok_or("m2: missing screen address")?
            .parse()
            .map_err(|_| "m2: invalid screen address")?;
        let mut screen = Screen::connect(address, true).await?;
        self.frames
            .lock()
            .map_err(|_| "m2: frame lock")?
            .push(screen.current()?);
        let frames = self.frames.clone();
        self.collector = Some(tokio::spawn(async move {
            loop {
                match screen.next().await {
                    Ok(frame) => frames.lock().map_err(|_| "m2: frame lock")?.push(frame),
                    Err(error) if error == "screen: stream ended" => return Ok(()),
                    Err(error) => return Err(error),
                }
            }
        }));
        Ok(())
    }

    async fn exercise(&mut self, utterance: &Path) -> Result<(), String> {
        let pcm = finite_pcm(utterance)?;
        let host = self.host.as_mut().ok_or("m2: missing host")?;
        host.send(&json!({"action":"finite","pcm":pcm})).await?;
        host.expect("host.ack").await?;
        self.settle().await?;
        let host = self.host.as_mut().ok_or("m2: missing host")?;
        host.send(&json!({"action":"playback"})).await?;
        let playback = host.expect("host.ack").await?;
        self.playback = self
            .relay
            .as_ref()
            .ok_or("m2: missing relay")?
            .timing()
            .audible_reply(&playback)?;
        Ok(())
    }

    async fn settle(&mut self) -> Result<(), String> {
        let mut done = false;
        let mut transcript = false;
        while !done {
            self.provider.as_mut().ok_or("m2: missing provider")?.tick().await?;
            let played = self.relay.as_ref().ok_or("m2: missing relay")?.timing().played;
            // Nine frames prove all eleven packets reached the real converter.
            // Transcript completion flushes its held final frame.
            if played >= 8640 && !transcript {
                self.provider
                    .as_mut()
                    .ok_or("m2: missing provider")?
                    .transcripts()
                    .await?;
                transcript = true;
            }
            done = advance(self.bridge.as_mut().ok_or("m2: missing bridge")?, 20).await?;
        }
        // Keep the remote's socket alive but stop its RTP driver before closing
        // the production media owner. The pin's sideband stays until idle stop.
        self.provider.as_mut().ok_or("m2: missing provider")?.quiesce().await?;
        advance(self.bridge.as_mut().ok_or("m2: missing bridge")?, 120_000).await?;
        self.bridge
            .as_mut()
            .ok_or("m2: missing bridge")?
            .expect("bridge.ready")
            .await?;
        Ok(())
    }

    async fn close_relay(&mut self, errors: &mut Vec<String>) {
        if let Some(relay) = self.relay.take() {
            errors.extend(relay.close().await.err());
        }
    }

    async fn close(&mut self) -> Result<Value, String> {
        let mut errors = vec![];
        if let Some(provider) = &self.provider {
            errors.extend(provider.quiesce().await.err());
        }
        if let Some(mut bridge) = self.bridge.take() {
            errors.extend(bridge.close().await.err());
        }
        if let Some(collector) = self.collector.take() {
            errors.extend(
                collector
                    .await
                    .map_err(|_| "m2: screen collector failed".to_owned())
                    .and_then(|r| r)
                    .err(),
            );
        }
        let mut observed = Value::Null;
        if let Some(provider) = self.provider.take() {
            match provider.close().await {
                Ok(snapshot) => observed = snapshot,
                Err(error) => errors.push(error),
            }
        }
        if let Some(mut host) = self.host.take() {
            errors.extend(host.close().await.err());
        }
        self.close_relay(&mut errors).await;
        if self.trace_path.is_file() {
            self.trace = read_trace(&self.trace_path)?;
        }
        if errors.is_empty() {
            Ok(observed)
        } else {
            Err(errors.join("; "))
        }
    }
}

pub(super) async fn run(options: &Options, root: &Path, utterance: &Path) -> (Value, Result<(), String>) {
    let mut state = Case::default();
    let output = root.join("private-success");
    let result = controlled(
        async {
            std::fs::create_dir(&output).map_err(|_| "m2: cannot own case directory")?;
            state.trace_path = output.join("bridge.private.jsonl");
            state.start(options).await?;
            state.exercise(utterance).await
        },
        options.cancel.as_deref(),
        tokio::time::Instant::now() + DEADLINE,
    )
    .await;
    let closed = state.close().await;
    let observed = closed.as_ref().cloned().unwrap_or(Value::Null);
    let measured = json!({"case":"success", "runtime":"codex", "credential_mode":"api_key",
        "calls":observed["calls"].as_array().map(Vec::len),
        "provider_wire":provider::valid(&observed), "refreshes":0, "provider":observed,
        "frames":state.frames.lock().ok().map(|f| f.clone()), "trace":state.trace.records,
        "reply_pcm":state.playback});
    let cleanup = if state.trace_path.is_file() {
        std::fs::remove_file(&state.trace_path)
            .and_then(|()| std::fs::remove_dir(&output))
            .map_err(|_| "m2: case directory cleanup".to_owned())
    } else if output.is_dir() {
        std::fs::remove_dir(&output).map_err(|_| "m2: case directory cleanup".into())
    } else {
        Ok(())
    };
    let result = result
        .and(closed.map(|_| ()))
        .and(cleanup)
        .and_then(|()| oracle::check(&measured));
    (measured, result)
}

fn read_trace(path: &Path) -> Result<Trace, String> {
    let mut trace = Trace::default();
    trace.append(&std::fs::read(path).map_err(|_| "m2: missing bridge trace")?)?;
    trace.finish()?;
    Ok(trace)
}

async fn host_ready(host: &mut Process) -> Result<Value, String> {
    let value = host.line().await?;
    match value["event"].as_str() {
        Some("host.ready") => Ok(value),
        Some("host.failed") => Err("m2: pinned hassmic-host startup failed before readiness".into()),
        _ => Err("m2: unexpected hassmic-host startup observation".into()),
    }
}

fn finite_pcm(utterance: &Path) -> Result<Vec<u8>, String> {
    let original = super::super::observers::bytes(utterance, 4096, "m2 utterance")?;
    if original.is_empty() || !original.len().is_multiple_of(2) {
        return Err("m2: invalid PCM utterance".into());
    }
    // The host consumes native 50ms blocks (1600 PCM16 bytes). Repeat the
    // nominated waveform, as the old continuous fixture did, without keeping
    // the capture source open indefinitely or padding the utterance with silence.
    let mut pcm = original.clone();
    while !pcm.len().is_multiple_of(1600) {
        if pcm.len() + original.len() > 4096 {
            return Err("m2: utterance cannot fill finite host blocks".into());
        }
        pcm.extend_from_slice(&original);
    }
    Ok(pcm)
}

async fn advance(bridge: &mut Process, milliseconds: u64) -> Result<bool, String> {
    bridge
        .send(&json!({"action":"advance_clock","milliseconds":milliseconds}))
        .await?;
    let mut completed = false;
    loop {
        let row = bridge.line().await?;
        match row["event"].as_str() {
            Some("bridge.clock_advanced") if row["milliseconds"] == milliseconds => return Ok(completed),
            Some("bridge.run_completed") if row["run_id"] == "run-1" => completed = true,
            _ => return Err("m2: unexpected bridge observation while advancing clock".into()),
        }
    }
}
