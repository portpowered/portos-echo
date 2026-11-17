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
        std::fs::create_dir(root).map_err(|_| "M3: private directory acquisition failed")?;
        self.root = root.into();
        self.host = Some(Process::start(process::host(python).arg("--finite"))?);
        let ready = self.host.as_mut().ok_or("M3: missing host")?.expect("host.ready");
        let address = process::admit_host(
            &mut self.endpoints,
            ready,
            process::readiness_deadline(),
            process::connect,
        )
        .await?;
        self.relay = Some(Relay::bind(address, &fixture()).await?);
        let relay = self.relay.as_ref().ok_or("M3: missing wire")?;
        self.live = Some(Live::bind(relay.observed.clone()).await?);
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
        production::configure(
            &mut command,
            root,
            relay.address,
            &self.live.as_ref().ok_or("M3: missing Live")?.origin,
            "http://127.0.0.1:1/mcp",
        )?;
        // M3 observes the same provider through the later native mute span.
        let config = root.join("configured.private/bridge.toml");
        let contents = std::fs::read_to_string(&config).map_err(|_| "M3: missing configuration")?;
        std::fs::write(
            config,
            contents.replace("session_idle_seconds = 0", "session_idle_seconds = 120"),
        )
        .map_err(|_| "M3: cannot retain Live session")?;
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
                return self.read_trace();
            }
            tokio::task::yield_now().await;
        }
    }

    pub async fn second(&mut self) -> Result<(), String> {
        self.control(json!({"action":"inject","pcm":fixture()})).await?;
        let old = self.wait("old reply Played", |s| s.played.len() == 1).await?;
        let played = self.control(json!({"action":"playback"})).await?;
        let old_pcm = played["pcm_bytes"]
            .as_u64()
            .ok_or("M3 cut: missing decoded old sink PCM")?;
        if usize::try_from(old_pcm).ok() != Some(old.bytes[0]) {
            return Err("M3 cut: host decoded PCM does not match the stopped old reply".into());
        }
        self.relay
            .as_ref()
            .ok_or("M3: missing wire")?
            .observed
            .send_modify(|state| state.old_sink_pcm = Some(old_pcm));
        self.wait("second answer Played after overlapping input", |s| s.played.len() == 2)
            .await?;
        let played = self.control(json!({"action":"playback"})).await?;
        self.second_pcm = Some(
            played["pcm_bytes"]
                .as_u64()
                .and_then(|count| count.checked_sub(old_pcm))
                .ok_or("M3 second answer: missing or backwards host PCM count")?,
        );
        self.bridge
            .as_mut()
            .ok_or("M3: missing bridge")?
            .expect("bridge.run_completed")
            .await?;
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

    pub async fn wait(&self, phase: &str, property: impl Fn(&Ledger) -> bool) -> Result<Ledger, String> {
        let relay = self.relay.as_ref().ok_or("M3: missing wire")?;
        let mut live = self.live.as_ref().ok_or("M3: missing Live")?.observed.subscribe();
        let failure = async {
            loop {
                if let Some(error) = live.borrow().failure.clone() {
                    return Err(error);
                }
                live.changed().await.map_err(|_| "M3: Live observer ended")?;
            }
        };
        tokio::select! {
            result = relay.wait(property) => result,
            result = failure => result,
            () = tokio::time::sleep(std::time::Duration::from_secs(5)) => Err(format!("M3 {phase}: missing observation before 5000 ms deadline")),
        }
    }

    pub fn read_trace(&mut self) -> Result<(), String> {
        let bytes = std::fs::read(self.root.join("bridge.private.jsonl")).map_err(|_| "M3: missing bridge trace")?;
        let mut trace = Trace::default();
        trace.append(&bytes)?;
        trace.finish()?;
        self.trace = trace;
        Ok(())
    }

    pub async fn close(&mut self) -> Result<(), String> {
        let mut failures = Vec::new();
        if let Some(live) = &self.live {
            live.prepare_close();
        }
        self.screen.take();
        if let Some(mut bridge) = self.bridge.take() {
            failures.extend(bridge.close().await.err());
            failures.extend(self.read_trace().err());
            for record in self.trace.records.iter().filter(|r| r["event"] == "bridge.error") {
                failures.push(format!("M3 bridge: {}", record["reason"]));
            }
        }
        if let Some(relay) = self.relay.take() {
            let observed = relay.observed.clone();
            failures.extend(relay.close().await.err());
            self.final_wire = Some(observed.borrow().clone());
        }
        if let Some(live) = self.live.take() {
            failures.extend(live.close().await.err());
        }
        if let Some(mut host) = self.host.take() {
            failures.extend(host.close().await.err());
        }
        if !self.root.as_os_str().is_empty() {
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
