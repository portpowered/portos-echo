//! One persistent run on the production bridge, pinned Codex helper and
//! native host. Every turn is delegated; replies are real Opus media.
use super::provider::{Control, Provider};
use crate::lab_run::sim::{
    m2::codex::{collect_playout, config::Root},
    m4::wire::{Relay, Timing},
    process::{self, Process},
    production,
    trace::Trace,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Eleven tone packets settle as ten 960-byte frames on the device.
const REPLY_BYTES: usize = 9600;
/// The sim clock never runs this far past a reply without an outcome.
const STALL_MS: u64 = 30_000;

#[derive(Default)]
pub(super) struct World {
    host: Option<Process>,
    relay: Option<Relay>,
    provider: Option<Provider>,
    bridge: Option<Process>,
    private: Option<Root>,
    root: PathBuf,
    pub clock: Value,
    pub trace: Trace,
    pub turns: usize,
    pub completed: bool,
    /// Wire session observations at each reply boundary, then at close.
    pub wire: Vec<Value>,
    pub snapshot: Value,
    /// Model requests with the tool outputs the pinned engine returned.
    pub models: Vec<Value>,
    pub endpoints: Vec<std::net::SocketAddr>,
    wait_for_long_answer: bool,
}

impl World {
    pub async fn start(
        &mut self,
        options: (&str, &Path),
        root: &Path,
        journey: super::Journey,
        script: &Value,
    ) -> Result<(), String> {
        let (python, bridge) = options;
        if !production::selected(bridge) {
            return Err("session: requires the prebuilt production echo-bridge".into());
        }
        self.private = Some(Root::create(root.into())?);
        self.root = root.into();
        self.host = Some(Process::start(process::host(python).arg("--finite"))?);
        let ready = self.host.as_mut().ok_or("session: missing host")?.expect("host.ready");
        let address = process::admit_host(
            &mut self.endpoints,
            ready,
            process::readiness_deadline(),
            process::connect,
        )
        .await?;
        let relay = Relay::bind_session(
            address,
            crate::lab_run::sim::m3::fixture(),
            journey == super::Journey::LongWait,
        )
        .await?;
        self.endpoints.push(relay.address);
        let provider = Provider::start(python, script).await?;
        let mut command = tokio::process::Command::new(bridge);
        crate::lab_run::sim::m3::configure(&mut command, root, relay.address.port(), &provider.origin)?;
        self.wait_for_long_answer = journey == super::Journey::Limits;
        if let Some(max) = journey.max() {
            let path = root.join("configured.private/bridge.toml");
            let text = std::fs::read_to_string(&path).map_err(|_| "session: missing configuration")?;
            if !text.contains("session_max_seconds = 600 ") {
                return Err("session: missing example run cap".into());
            }
            let mut text = text.replace("session_max_seconds = 600 ", &format!("session_max_seconds = {max} "));
            if self.wait_for_long_answer {
                let home = root.join("agent-home");
                std::fs::create_dir(&home).map_err(|_| "session: cannot create settings home")?;
                std::fs::write(home.join("settings.toml"), "[agent]\nmax_tool_calls = 4\n")
                    .map_err(|_| "session: cannot seed V1 settings overlay")?;
                use std::fmt::Write as _;
                write!(text, "\n[home]\ndir = '{}'\nagent_tools = false\n", home.display())
                    .map_err(|_| "session: cannot configure settings home")?;
            }
            std::fs::write(path, text).map_err(|_| "session: cannot configure run cap")?;
        }
        self.relay = Some(relay);
        self.provider = Some(provider);
        self.bridge = Some(Process::start(&mut command)?);
        self.bridge
            .as_mut()
            .ok_or("session: missing bridge")?
            .expect("bridge.ready")
            .await?;
        if journey == super::Journey::LongWait {
            self.control(json!({"action":"registered"})).await?;
            self.reconnect().await?;
        }
        self.control(json!({"action":"registered"})).await.map(|_| ())
    }

    async fn control(&mut self, value: Value) -> Result<Value, String> {
        let host = self.host.as_mut().ok_or("session: missing host")?;
        host.send(&value).await?;
        host.expect("host.ack").await
    }

    async fn reconnect(&mut self) -> Result<(), String> {
        self.relay.as_mut().ok_or("session: missing relay")?.reconnect()?;
        let started = self.epoch();
        while !self.relay()?.reconnected()? {
            self.step(20).await?;
            if self.epoch() > started + STALL_MS {
                return Err(self.stalled("reconnect registration"));
            }
        }
        Ok(())
    }

    /// Speak one utterance and settle its delegated reply. The first wakes the
    /// device; later ones continue capture in the same run without a wake.
    pub async fn turn(&mut self) -> Result<(), String> {
        let index = self.turns;
        let pcm = crate::lab_run::sim::m3::fixture();
        if index == 0 {
            self.control(json!({"action":"finite","pcm":pcm})).await?;
        } else {
            let distinct: Vec<u8> = pcm.iter().map(|byte| !byte).collect();
            self.control(json!({"action":"inject","pcm":distinct})).await?;
        }
        self.settle(index).await?;
        self.turns += 1;
        let observation = self.relay()?.timing().session_observation();
        self.wire.push(observation);
        Ok(())
    }

    fn relay(&self) -> Result<&Relay, String> {
        self.relay.as_ref().ok_or_else(|| "session: missing wire".into())
    }

    async fn settle(&mut self, index: usize) -> Result<(), String> {
        let mut gates = Gates::new(index);
        let mut started = self.epoch();
        loop {
            let provider = self.provider.as_mut().ok_or("session: missing provider")?;
            // RTP and transcript gates follow the media cadence; once the reply
            // is received only the bridge's own clock remains.
            if !gates.idle {
                provider.tick().await?;
            }
            let timing = self.relay.as_ref().ok_or("session: missing wire")?.timing();
            let admitted = index > 0 || (connected(&self.clock) && timing.endpoint_seen());
            let milliseconds = gates.advance(provider, &timing, admitted).await?;
            if self.wait_for_long_answer {
                self.release_long_answer().await?;
                if !self.wait_for_long_answer {
                    started = self.epoch();
                }
            }
            self.step(milliseconds).await?;
            if gates.idle && self.played()? > index {
                return Ok(());
            }
            if self.epoch() > started + STALL_MS {
                return Err(self.stalled(&format!("reply {index}")));
            }
        }
    }

    async fn release_long_answer(&mut self) -> Result<(), String> {
        let provider = self.provider.as_ref().ok_or("session: missing provider")?;
        if provider.count("waiting") == 0 {
            self.read_trace()?;
            if self.trace.records.iter().any(|row| row["event"] == "turn/interrupt") {
                let applied = self
                    .trace
                    .records
                    .iter()
                    .filter(|row| row["event"] == "session.tool_receipt")
                    .count();
                return Err(format!(
                    "session: admitted long turn interrupted after {applied} of {} tool effects",
                    super::LONG_CALLS
                ));
            }
            return Ok(());
        }
        // The pin has acknowledged all twelve calls and is waiting on its
        // last model response. Advance only the bridge's injected clock.
        self.step(60_000).await?;
        self.step(super::LONG_WAIT_MS - 60_000).await?;
        self.provider
            .as_ref()
            .ok_or("session: missing provider")?
            .control(Control::ReleaseGoodbye)
            .await?;
        self.wait_for_long_answer = false;
        Ok(())
    }

    async fn step(&mut self, milliseconds: u64) -> Result<(), String> {
        let bridge = self.bridge.as_mut().ok_or("session: missing bridge")?;
        if collect_playout(bridge, &mut self.clock, milliseconds).await?.is_some() {
            self.completed = true;
        }
        Ok(())
    }

    fn epoch(&self) -> u64 {
        self.clock["epoch_ms"].as_u64().unwrap_or(0)
    }

    fn played(&mut self) -> Result<usize, String> {
        self.read_trace()?;
        Ok(self
            .trace
            .records
            .iter()
            .filter(|r| r["event"] == "reply.played")
            .count())
    }

    fn stalled(&self, phase: &str) -> String {
        let observed = self
            .provider
            .as_ref()
            .map(|provider| provider.observed.borrow().clone());
        let timing = self.relay.as_ref().map(|relay| relay.timing().session_observation());
        let events: Vec<_> = self.trace.records.iter().map(|r| r["event"].clone()).collect();
        format!(
            "session {phase}: no outcome within {STALL_MS} sim ms; provider={observed:?} wire={timing:?} trace={events:?}"
        )
    }

    /// Advance the bridge's clock until it ends the run by itself.
    pub async fn finish(&mut self) -> Result<(), String> {
        let started = self.epoch();
        while !self.completed {
            self.step(20).await?;
            if self.epoch() >= started + 31_000 && self.wire.len() == 1 {
                let mut checkpoint = self.relay()?.timing().session_observation();
                checkpoint["checkpoint_epoch_ms"] = json!(self.epoch());
                self.wire.push(checkpoint);
            }
            if self.epoch() > started + 65_000 {
                self.read_trace()?;
                return Err(self.stalled("run end"));
            }
        }
        self.relay()?.wait_session(self.turns, 1).await?;
        self.read_trace()
    }

    /// Read while the bridge may still be appending a record.
    pub fn read_trace(&mut self) -> Result<(), String> {
        let mut trace = Trace::default();
        trace.append(
            &std::fs::read(self.root.join("bridge.private.jsonl")).map_err(|_| "session: missing bridge trace")?,
        )?;
        self.trace = trace;
        Ok(())
    }

    pub async fn close(&mut self) -> Result<(), String> {
        let mut failures = Vec::new();
        if let Some(provider) = &self.provider {
            failures.extend(provider.quiesce().await.err());
        }
        if let Some(mut bridge) = self.bridge.take() {
            failures.extend(bridge.close().await.err());
            failures.extend(self.read_trace().and_then(|()| self.trace.finish()).err());
            failures.extend(self.retain_trace().err());
            for record in self.trace.records.iter().filter(|r| r["event"] == "bridge.error") {
                failures.push(format!("session bridge: {}", record["reason"]));
            }
        }
        if let Some(relay) = self.relay.take() {
            match relay.close().await {
                Ok(timing) => self.wire.push(timing.session_observation()),
                Err(error) => failures.push(error),
            }
        }
        if let Some(provider) = self.provider.take() {
            self.models = provider.observed.borrow()["models"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            match provider.close().await {
                Ok(snapshot) => self.snapshot = snapshot,
                Err(error) => failures.push(error),
            }
        }
        if let Some(mut host) = self.host.take() {
            failures.extend(host.close().await.err());
        }
        self.private.take();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    fn retain_trace(&self) -> Result<(), String> {
        let parent = self.root.parent().ok_or("session: missing artifact parent")?;
        std::fs::copy(
            self.root.join("bridge.private.jsonl"),
            parent.join("session.trace.jsonl"),
        )
        .map(|_| ())
        .map_err(|_| "session: cannot retain payload-free bridge trace".into())
    }
}

/// Provider gates for one delegated reply, keyed to forwarded device bytes.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum Phase {
    Admitting,
    Delegated,
    Released,
    Tail,
    Spoken,
}

struct Gates {
    index: usize,
    base: usize,
    phase: Phase,
    idle: bool,
}

impl Gates {
    fn new(index: usize) -> Self {
        Self {
            index,
            base: index * REPLY_BYTES,
            phase: Phase::Admitting,
            idle: false,
        }
    }

    /// Open the next gate when its condition holds and return the next clock
    /// step: zero while the released reply flows, else one media frame.
    async fn advance(&mut self, provider: &Provider, timing: &Timing, admitted: bool) -> Result<u64, String> {
        let played = timing.played;
        let (control, next) = match self.phase {
            Phase::Admitting if admitted => (Control::Delegate(self.index), Phase::Delegated),
            Phase::Delegated if provider.count("appends") > self.index => (Control::Audio(0..5), Phase::Released),
            Phase::Released if played >= self.base + 2880 => (Control::Audio(5..11), Phase::Tail),
            Phase::Tail if played >= self.base + 8640 => (Control::Transcripts(self.index), Phase::Spoken),
            _ => return Ok(self.clock(timing)),
        };
        provider.control(control).await?;
        self.phase = next;
        Ok(self.clock(timing))
    }

    /// Device playout runs on the wall clock: keep the media cadence until
    /// the device reports this reply Played, then only the bridge clock remains.
    fn clock(&mut self, timing: &Timing) -> u64 {
        let played = timing.played;
        self.idle =
            self.phase == Phase::Spoken && played >= self.base + REPLY_BYTES && timing.replies_played() > self.index;
        if self.phase >= Phase::Released && played < self.base + REPLY_BYTES {
            0
        } else {
            20
        }
    }
}

fn connected(clock: &Value) -> bool {
    clock["states"]
        .as_array()
        .is_some_and(|states| states.iter().any(|row| row["state"]["state"] == "connected"))
}
