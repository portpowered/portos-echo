use super::super::{
    process::{self, Process},
    production::{self, Screen},
    trace::Trace,
};
use super::{
    live::Live,
    wire::{Ledger, Relay},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct World {
    pub relay: Option<Relay>,
    pub live: Option<Live>,
    host: Option<Process>,
    bridge: Option<Process>,
    pub screen: Option<Screen>,
    pub trace: Trace,
    pub first_pcm: Option<u64>,
    pub second_pcm: Option<u64>,
    pub muted_live_before: Option<u64>,
    pub muted_live_after: Option<u64>,
    pub final_wire: Option<Ledger>,
    pub endpoints: Vec<std::net::SocketAddr>,
    root: PathBuf,
    #[cfg(test)]
    released: Vec<&'static str>,
    private: Option<super::super::m2::codex::config::Root>,
    clock: Value,
    completed: bool,
}

// A finite deterministic marker, interpreted only by the synthetic scenario.
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
    pub async fn start(&mut self, python: &str, bridge: &Path, root: &Path) -> Result<(), String> {
        if !production::selected(bridge) {
            return Err("M3 requires the prebuilt production echo-bridge".into());
        }
        self.private = Some(super::super::m2::codex::config::Root::create(root.into())?);
        self.root = root.into();
        self.host = Some(Process::start(process::host(python).arg("--finite"))?);
        let ready = host_ready(self.host.as_mut().ok_or("M3: missing host")?);
        let address = process::admit_host(
            &mut self.endpoints,
            ready,
            process::readiness_deadline(),
            process::connect,
        )
        .await?;
        self.relay = Some(Relay::bind(address, &fixture()).await?);
        let relay = self.relay.as_ref().ok_or("M3: missing wire")?;
        self.live = Some(Live::bind(python, relay.observed.clone()).await?);
        self.endpoints.push(relay.address);
        self.endpoints.push(
            self.live
                .as_ref()
                .ok_or("M3: missing Live")?
                .origin
                .trim_start_matches("http://")
                .parse()
                .map_err(|_| "M3: invalid Live endpoint")?,
        );
        let mut command = tokio::process::Command::new(bridge);
        configure(
            &mut command,
            root,
            relay.address.port(),
            &self.live.as_ref().ok_or("M3: missing Live")?.origin,
        )?;
        self.bridge = Some(Process::start(&mut command)?);
        self.bridge
            .as_mut()
            .ok_or("M3: missing bridge")?
            .expect("bridge.ready")
            .await?;
        self.control(json!({"action":"registered"})).await?;
        self.read_trace()?;
        self.listeners().await
    }

    async fn listeners(&mut self) -> Result<(), String> {
        let address = self
            .trace
            .records
            .iter()
            .find(|r| r["event"] == "screen.listening")
            .and_then(|r| r["address"].as_str())
            .ok_or("M3: missing screen listener")?
            .parse()
            .map_err(|_| "M3: invalid screen listener")?;
        self.screen = Some(Screen::connect(address, true).await?);
        self.endpoints.push(address);
        let health = self
            .trace
            .records
            .iter()
            .find(|r| r["event"] == "bridge.started")
            .and_then(|r| r["health_addr"].as_str())
            .ok_or("M3: missing health listener")?
            .parse()
            .map_err(|_| "M3: invalid health listener")?;
        self.endpoints.push(health);
        Ok(())
    }

    pub async fn control(&mut self, value: Value) -> Result<Value, String> {
        let host = self.host.as_mut().ok_or("M3: missing host")?;
        host.send(&value).await?;
        host.expect("host.ack").await
    }

    pub async fn first(&mut self) -> Result<(), String> {
        self.control(json!({"action":"finite","pcm":fixture()})).await?;
        self.wait("first reply PCM", |s| s.bytes[0] >= 960).await?;
        loop {
            let played = self.control(json!({"action":"playback"})).await?;
            let bytes = played["pcm_bytes"]
                .as_u64()
                .ok_or("M3 first reply: missing host PCM count")?;
            if bytes > 0 {
                self.first_pcm = Some(bytes);
                return self.first_started().await;
            }
            tokio::task::yield_now().await;
        }
    }

    async fn first_started(&mut self) -> Result<(), String> {
        // PCM can reach the device before the bridge observes its send receipt.
        // Speaking is published after reply.started is written, so it is the
        // causal barrier for the next exact trace assertion.
        let screen = self.screen.as_mut().ok_or("M3: missing screen")?;
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            screen.expect(&json!({"state":"speaking"})),
        )
        .await
        .map_err(|_| "M3 first reply: missing Speaking before 5000 ms deadline")??;
        self.read_trace()
    }

    pub async fn second(&mut self) -> Result<(), String> {
        let baseline = self.live.as_ref().ok_or("M3: missing Live")?.snapshot().await?;
        self.relay
            .as_ref()
            .ok_or("M3: missing wire")?
            .observed
            .send_modify(|state| {
                state.overlap_base = Some(baseline);
                state.stimulus = Some(tokio::time::Instant::now());
            });
        self.control(json!({"action":"inject","pcm":fixture()})).await?;
        let old = self.wait("old reply Played", |s| s.played.len() == 1).await?;
        // Socket Played and the complete native stderr record have separate readers.
        let played = self.control(json!({"action":"playback","wait_flush":true})).await?;
        let old_pcm = played["pcm_bytes"]
            .as_u64()
            .ok_or("M3 cut: missing decoded old sink PCM")?;
        let flush = played["flush"].clone();
        super::oracle::flush(&flush, old.bytes[0], old_pcm)?;
        // The prefix triggered cancellation. Its distinguishable tail is fed
        // only after native Played, without another wake or run-pipeline.
        self.control(json!({"action":"inject","pcm":fixture().iter().map(|byte| !byte).collect::<Vec<_>>()}))
            .await?;
        self.relay
            .as_ref()
            .ok_or("M3: missing wire")?
            .observed
            .send_modify(|state| {
                state.old_sink_pcm = Some(old_pcm);
                state.flush_observation = Some(flush);
            });
        self.wait("second answer Played and explicit end", |s| {
            s.played.len() == 2 && s.ends == 1
        })
        .await?;
        let played = self.control(json!({"action":"playback"})).await?;
        self.second_pcm = Some(
            played["pcm_bytes"]
                .as_u64()
                .and_then(|count| count.checked_sub(old_pcm))
                .ok_or("M3 second answer: missing or backwards host PCM count")?,
        );
        while !self.completed {
            self.completed = super::super::m2::codex::collect_playout(
                self.bridge.as_mut().ok_or("M3: missing bridge")?,
                &mut self.clock,
                20,
            )
            .await?
            .is_some();
        }
        self.read_trace()
    }

    pub async fn press(&mut self) -> Result<(), String> {
        let muted = self.relay.as_ref().ok_or("M3: missing wire")?.observed.borrow().muted;
        self.control(json!({"action":"mute"})).await?;
        self.wait("native mute transition", |s| s.muted != muted).await?;
        self.screen
            .as_mut()
            .ok_or("M3: missing screen")?
            .expect(&json!({"state":if muted {"ready"} else {"muted"}}))
            .await?;
        let frames = self.live.as_ref().ok_or("M3: missing Live")?.snapshot().await?;
        if muted {
            self.muted_live_after = Some(frames);
        } else {
            self.muted_live_before = Some(frames);
        }
        self.read_trace()
    }

    pub async fn muted_input(&mut self) -> Result<(), String> {
        self.control(json!({"action":"muted","pcm":fixture()})).await?;
        Ok(())
    }

    pub async fn wait(&mut self, phase: &str, property: impl Fn(&Ledger) -> bool) -> Result<Ledger, String> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(20));
        loop {
            let state = self.relay.as_ref().ok_or("M3: missing wire")?.observed.borrow().clone();
            self.relay.as_ref().ok_or("M3: missing wire")?.wait(|_| true).await?;
            if property(&state) {
                return Ok(state);
            }
            if let Some(error) = self
                .live
                .as_ref()
                .ok_or("M3: missing Live")?
                .observed
                .borrow()
                .failure
                .clone()
            {
                return Err(error);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "M3 {phase}: missing observation before 5000 ms deadline; capture_chunks={} Live_frames={} capture_tail_bytes={} continues={}",
                    state.capture_frames,
                    self.live.as_ref().ok_or("M3: missing Live")?.observed.borrow().frames,
                    state.tail_bytes,
                    state.continues
                ));
            }
            tick.tick().await;
            let result = super::super::m2::codex::collect_playout(
                self.bridge.as_mut().ok_or("M3: missing bridge")?,
                &mut self.clock,
                20,
            )
            .await?;
            self.completed |= result.is_some();

        }
    }

    pub fn read_trace(&mut self) -> Result<(), String> {
        let bytes = std::fs::read(self.root.join("bridge.private.jsonl")).map_err(|_| "M3: missing bridge trace")?;
        let mut trace = Trace::default();
        trace.append(&bytes)?;
        trace.finish()?;
        if let Some(relay) = &self.relay {
            trace.records.extend(relay.observed.borrow().observations());
        }
        self.trace = trace;
        Ok(())
    }

    fn retain_trace(&self) -> Result<(), String> {
        let parent = self.root.parent().ok_or("M3: missing artifact parent")?;
        std::fs::copy(self.root.join("bridge.private.jsonl"), parent.join("m3.trace.jsonl"))
            .map(|_| ())
            .map_err(|_| "M3: cannot retain payload-free bridge trace".into())
    }

    async fn idle(&mut self) -> Result<(), String> {
        self.live.as_ref().ok_or("M3: missing Live")?.quiesce().await?;
        let bridge = self.bridge.as_mut().ok_or("M3: missing bridge")?;
        bridge
            .send(&json!({"action":"advance_clock","milliseconds":120_000}))
            .await?;
        let advanced = bridge.expect("bridge.clock_advanced").await?;
        if advanced["milliseconds"] != 120_000 {
            return Err("M3: invalid idle clock receipt".into());
        }
        bridge.expect("bridge.ready").await?;
        self.read_trace()?;
        let closed: Vec<_> = self
            .trace
            .records
            .iter()
            .filter(|row| row["event"] == "live.closed")
            .collect();
        if closed.len() != 1 || closed[0]["reason"] != "idle" {
            return Err("M3: expected exactly one healthy idle close".into());
        }
        Ok(())
    }

    pub async fn close(&mut self) -> Result<(), String> {
        let mut failures = Vec::new();
        if self.second_pcm.is_some() {
            failures.extend(self.idle().await.err());
        }
        if let Some(live) = &self.live {
            live.prepare_close();
        }
        self.screen.take();
        if let Some(mut bridge) = self.bridge.take() {
            let closed = bridge.close().await;
            #[cfg(test)]
            self.record_release("bridge", &closed);
            failures.extend(closed.err());
            failures.extend(self.read_trace().err());
            failures.extend(self.retain_trace().err());
            for record in self.trace.records.iter().filter(|r| r["event"] == "bridge.error") {
                failures.push(format!("M3 bridge: {}", record["reason"]));
            }
        }
        if let Some(relay) = self.relay.take() {
            let observed = relay.observed.clone();
            let closed = relay.close().await;
            #[cfg(test)]
            self.record_release("relay", &closed);
            failures.extend(closed.err());
            self.final_wire = Some(observed.borrow().clone());
        }
        if let Some(live) = self.live.take() {
            let closed = live.close().await;
            #[cfg(test)]
            self.record_release("live", &closed);
            failures.extend(closed.err());
        }
        if let Some(mut host) = self.host.take() {
            let closed = host.close().await;
            #[cfg(test)]
            self.record_release("host", &closed);
            failures.extend(closed.err());
        }
        self.private.take();
        if !self.root.as_os_str().is_empty() && self.root.exists() {
            failures.extend(
                std::fs::remove_dir_all(&self.root)
                    .err()
                    .map(|_| "cleanup: M3 private directory removal failed".into()),
            );
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

#[cfg(test)]
#[path = "lifecycle.rs"]
mod tests;
fn configure(command: &mut tokio::process::Command, root: &Path, port: u16, origin: &str) -> Result<(), String> {
    production::configure(
        command,
        root,
        ([127, 0, 0, 1], port).into(),
        origin,
        "http://127.0.0.1:1/mcp",
    )?;
    let path = root.join("configured.private/bridge.toml");
    let template = std::fs::read_to_string(&path).map_err(|_| "M3: missing configuration")?;
    let start = template.find("[[mcp.server]]").ok_or("M3: missing example MCP")?;
    let end = start + template[start..].find("[screen]").ok_or("M3: missing example screen")?;
    let text = format!("{}[mcp]\nserver = []\n\n{}", &template[..start], &template[end..])
        .replace("voice = \"alloy\"\n", "")
        .replace("session_idle_seconds = 0", "session_idle_seconds = 120");
    std::fs::write(
        path,
        format!("{text}\n[codex]\nhome = '{}'\n", root.join("home").display()),
    )
    .map_err(|_| "M3: cannot configure Codex")?;
    command.arg("--sim-clock");
    Ok(())
}

async fn host_ready(host: &mut Process) -> Result<Value, String> {
    let value = host.line().await?;
    match value["event"].as_str() {
        Some("host.ready") => Ok(value),
        Some("host.failed") => Err("M3: pinned hassmic-host startup failed before readiness".into()),
        _ => Err("M3: unexpected hassmic-host startup observation".into()),
    }
}
