//! One persistent run on the production bridge, pinned Codex helper and
//! native host. Every turn is delegated; replies are real Opus media.
use super::{
    duplex::Satellite,
    provider::{Control, Provider},
};
#[path = "recovery/long_wait.rs"]
mod long_wait;
#[path = "recovery/recovery.rs"]
mod recovery;
use crate::lab_run::sim::{
    m2::codex::{collect_playout, collect_run_playout, config::Root},
    m4::wire::{Relay, Timing},
    process::{self, Process},
    production,
    trace::Trace,
};
use recovery::MediaBoundary;
use serde_json::{Value, json};
use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

/// Eleven tone packets settle as ten 960-byte frames on the device.
const REPLY_BYTES: usize = 9600;
/// The sim clock never runs this far past a reply without an outcome.
const STALL_MS: u64 = 30_000;

#[derive(Default)]
pub(super) struct World {
    host: Option<Process>,
    relay: Option<Relay>,
    satellite: Option<Satellite>,
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
    question: Option<long_wait::Question>,
    recovery: bool,
    media_boundary: Option<MediaBoundary>,
    journey: Option<super::Journey>,
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
        self.recovery = journey.recovers();
        self.journey = Some(journey);
        self.media_boundary = (journey == super::Journey::ActiveMedia).then_some(MediaBoundary::Waiting);
        let address = self.start_device(python, journey).await?;
        let provider = Provider::start(python, script).await?;
        let mut command = tokio::process::Command::new(bridge);
        crate::lab_run::sim::m3::configure(&mut command, root, address.port(), &provider.origin)?;
        self.wait_for_long_answer = journey == super::Journey::Limits;
        self.question = (journey == super::Journey::LongQuestion).then_some(long_wait::Question::Speaking);
        if let Some(max) = journey.max() {
            configure_run(root, max, self.wait_for_long_answer)?;
        }
        self.provider = Some(provider);
        self.bridge = Some(Process::start_bridge(&mut command, false)?);
        self.bridge
            .as_mut()
            .ok_or("session: missing bridge")?
            .expect("bridge.ready")
            .await?;
        if journey == super::Journey::LongWait {
            self.control(json!({"action":"registered"})).await?;
            self.reconnect().await?;
        }
        if self.recovery {
            return Ok(());
        }
        self.control(json!({"action":"registered"})).await.map(|_| ())
    }

    async fn start_device(&mut self, python: &str, journey: super::Journey) -> Result<std::net::SocketAddr, String> {
        if self.recovery {
            let satellite = Satellite::bind().await?;
            let address = satellite.address;
            self.endpoints.push(address);
            self.satellite = Some(satellite);
            return Ok(address);
        }
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
        let address = relay.address;
        self.endpoints.push(address);
        self.relay = Some(relay);
        Ok(address)
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
        if let Some(satellite) = &self.satellite {
            satellite.speak(true).await?;
        } else if index == 0 {
            self.control(json!({"action":"finite","pcm":pcm})).await?;
        } else {
            let distinct: Vec<u8> = pcm.iter().map(|byte| !byte).collect();
            self.control(json!({"action":"inject","pcm":distinct})).await?;
        }
        if self.recovery {
            if index == 0 {
                self.abandon_stuck_turn().await?;
            } else {
                self.wait_for_call(2).await?;
                if self.media_boundary.is_some() {
                    self.prepare_playback().await?;
                }
                self.settle(index, 0).await?;
            }
        } else {
            self.settle(index, index).await?;
        }
        self.turns += 1;
        let observation = self.timing()?.session_observation();
        self.wire.push(observation);
        Ok(())
    }

    /// The delegated turn ends the session before anything is voiced: the
    /// turn settles when the run ends, with no reply to play.
    fn ends_unvoiced(&self) -> bool {
        self.journey == Some(super::Journey::End)
    }

    fn relay(&self) -> Result<&Relay, String> {
        self.relay.as_ref().ok_or_else(|| "session: missing wire".into())
    }

    fn timing(&self) -> Result<Timing, String> {
        if let Some(satellite) = &self.satellite {
            Ok(satellite.observed.borrow().clone())
        } else {
            self.relay().map(Relay::timing)
        }
    }

    async fn settle(&mut self, index: usize, replies: usize) -> Result<(), String> {
        let mut gates = Gates::new(index, replies);
        if self.playback_pending() {
            gates.phase = Phase::Delegated;
        }
        let mut started = self.epoch();
        loop {
            let timing = self.timing()?;
            self.cross_playback(&timing).await?;
            let playback_pending = self.playback_pending();
            let admitted = index > 0 || (connected(&self.clock) && timing.endpoint_seen());
            if self.long_wait(admitted && gates.phase == Phase::Admitting).await? {
                started = self.epoch();
            }
            let provider = self.provider.as_mut().ok_or("session: missing provider")?;
            // RTP and transcript gates follow the media cadence; once the reply
            // is received only the bridge's own clock remains.
            if !gates.idle {
                provider.tick().await?;
            }
            let milliseconds = gates.advance(provider, &timing, admitted, playback_pending).await?;
            self.step(milliseconds).await?;
            if gates.idle && self.played()? > replies {
                return Ok(());
            }
            if self.ends_unvoiced() && self.completed {
                return Ok(());
            }
            if self.epoch() > started + STALL_MS {
                return Err(self.stalled(&format!("reply {index}")));
            }
        }
    }

    async fn step(&mut self, milliseconds: u64) -> Result<(), String> {
        let bridge = self.bridge.as_mut().ok_or("session: missing bridge")?;
        let completed = if self.recovery {
            collect_run_playout(bridge, &mut self.clock, milliseconds, "run-2", true).await?
        } else {
            collect_playout(bridge, &mut self.clock, milliseconds).await?
        };
        if completed.is_some() {
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
        let timing = self.timing().map(|timing| timing.session_observation());
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
        if !self.recovery {
            let replies = if self.ends_unvoiced() { 0 } else { self.turns };
            self.relay()?.wait_session(replies, 1).await?;
        }
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
        self.close_bridge(&mut failures).await;
        if let Some(relay) = self.relay.take() {
            match relay.close().await {
                Ok(timing) => self.wire.push(timing.session_observation()),
                Err(error) => failures.push(error),
            }
        }
        if let Some(mut satellite) = self.satellite.take() {
            failures.extend(satellite.close().await.err());
            self.wire.push(satellite.observed.borrow().session_observation());
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
        match failures.as_slice() {
            [] => Ok(()),
            _ => Err(failures.join("; ")),
        }
    }

    async fn close_bridge(&mut self, failures: &mut Vec<String>) {
        if let Some(mut bridge) = self.bridge.take() {
            failures.extend(bridge.close().await.err());
            failures.extend(self.read_trace().and_then(|()| self.trace.finish()).err());
            failures.extend(self.retain_trace().err());
            for record in self.trace.records.iter().filter(|r| r["event"] == "bridge.error") {
                failures.push(format!("session bridge: {}", record["reason"]));
            }
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

fn configure_run(root: &Path, max: u64, seed_v1: bool) -> Result<(), String> {
    let path = root.join("configured.private/bridge.toml");
    let text = std::fs::read_to_string(&path).map_err(|_| "session: missing configuration")?;
    if !text.contains("session_max_seconds = 600 ") {
        return Err("session: missing example run cap".into());
    }
    let mut text = text.replace("session_max_seconds = 600 ", &format!("session_max_seconds = {max} "));
    if seed_v1 {
        text = text.replacen(
            "[agent]\n",
            "[agent]\nmax_tool_calls = 8\ntask_deadline_seconds = 60\n",
            1,
        );
        let home = root.join("agent-home");
        std::fs::create_dir(&home).map_err(|_| "session: cannot create settings home")?;
        std::fs::write(home.join("settings.toml"), "[agent]\nmax_tool_calls = 4\n")
            .map_err(|_| "session: cannot seed V1 settings overlay")?;
        write!(text, "\n[home]\ndir = '{}'\nagent_tools = false\n", home.display())
            .map_err(|_| "session: cannot configure settings home")?;
    }
    std::fs::write(path, text).map_err(|_| "session: cannot configure run cap".into())
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
    replies: usize,
    base: usize,
    phase: Phase,
    idle: bool,
}

impl Gates {
    fn new(index: usize, replies: usize) -> Self {
        Self {
            index,
            replies,
            base: replies * REPLY_BYTES,
            phase: Phase::Admitting,
            idle: false,
        }
    }

    /// Open the next gate when its condition holds and return the next clock
    /// step: zero while the released reply flows, else one media frame.
    async fn advance(
        &mut self,
        provider: &Provider,
        timing: &Timing,
        admitted: bool,
        playback_pending: bool,
    ) -> Result<u64, String> {
        let played = timing.played;
        let (control, next) = match self.phase {
            Phase::Admitting if admitted => (Control::Delegate(self.index), Phase::Delegated),
            Phase::Delegated
                if provider.observed.borrow()["appends"]
                    .as_array()
                    .is_some_and(|appends| appends.contains(&json!(format!("session-turn-{}", self.index)))) =>
            {
                (Control::Audio(0..5), Phase::Released)
            }
            Phase::Released if played >= self.base + 2880 => (Control::Audio(5..11), Phase::Tail),
            Phase::Tail if played >= self.base + 8640 => (Control::Transcripts(self.index), Phase::Spoken),
            _ => return Ok(if playback_pending { 0 } else { self.clock(timing) }),
        };
        if playback_pending && next == Phase::Released {
            provider
                .control(Control::Events(
                    "active-playback",
                    vec![json!({
                        "type":"output_transcript.added","item":{"text":"The new wake was answered."}
                    })],
                ))
                .await?;
        }
        provider.control(control).await?;
        self.phase = next;
        Ok(self.clock(timing))
    }

    /// Device playout runs on the wall clock: keep the media cadence until
    /// the device reports this reply Played, then only the bridge clock remains.
    fn clock(&mut self, timing: &Timing) -> u64 {
        let played = timing.played;
        self.idle =
            self.phase == Phase::Spoken && played >= self.base + REPLY_BYTES && timing.replies_played() > self.replies;
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
