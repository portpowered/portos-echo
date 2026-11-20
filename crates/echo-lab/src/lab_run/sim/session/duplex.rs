//! Shared duplex fixture for Live-originated replies on the pinned app server.
mod satellite;

use super::provider::{Control, Provider};
use crate::lab_run::{
    run_options::Options,
    sim::{
        m2::codex::{collect_playout_for, config::Root},
        process::Process,
        production::{self, Screen},
        trace::Trace,
    },
};
use satellite::Satellite;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::time::Instant;

pub(super) struct World {
    bridge: Process,
    pub(super) satellite: Satellite,
    pub(super) provider: Provider,
    screen: Screen,
    root: Root,
    pub(super) clock: Value,
    pub(super) trace: Trace,
    pub(super) run_id: &'static str,
}

impl World {
    pub async fn start(
        options: &Options,
        output: PathBuf,
        script: &Value,
        configure: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<Self, String> {
        let bridge = options
            .bridge
            .as_deref()
            .filter(|path| production::selected(path))
            .ok_or("duplex: requires the prebuilt production bridge")?;
        let root = Root::create(output)?;
        let satellite = Satellite::bind().await?;
        let provider = Provider::start(&options.python, script).await?;
        let mut command = tokio::process::Command::new(bridge);
        crate::lab_run::sim::m3::configure(&mut command, &root.0, satellite.address.port(), &provider.origin)?;
        configure(&root.0)?;
        let mut bridge = Process::start(&mut command)?;
        bridge.expect("bridge.ready").await?;
        let trace = read_trace(&root.0)?;
        let screen_address = trace
            .records
            .iter()
            .find(|row| row["event"] == "screen.listening")
            .and_then(|row| row["address"].as_str())
            .ok_or("duplex: missing paired screen")?
            .parse()
            .map_err(|_| "duplex: invalid screen address")?;
        let screen = Screen::connect(screen_address, true).await?;
        Ok(Self {
            bridge,
            satellite,
            provider,
            screen,
            root,
            clock: Value::Null,
            trace,
            run_id: "run-1",
        })
    }

    pub async fn connected(&mut self) -> Result<(), String> {
        self.wait(
            "Live connection",
            |world| {
                world.clock["states"]
                    .as_array()
                    .is_some_and(|states| states.iter().any(|row| row["state"]["state"] == "connected"))
            },
            20,
        )
        .await
    }

    pub async fn start_pcm(&mut self, index: usize) -> Result<usize, String> {
        let start_bytes = self.satellite.observed.borrow().played;
        // Control and RTP travel independently. Wait for the device's
        // audio-start before sending the finite audio burst.
        self.wait(
            "Wyoming reply start",
            |world| world.satellite.observed.borrow().reply_frames().len() == index + 1,
            0,
        )
        .await?;
        self.provider.control(Control::Audio(0..5)).await?;
        self.wait(
            "first reply PCM",
            |world| world.satellite.observed.borrow().played >= start_bytes + 2_880,
            0,
        )
        .await?;
        Ok(start_bytes)
    }

    pub async fn tail_pcm(&mut self, start_bytes: usize) -> Result<(), String> {
        self.provider.control(Control::Audio(5..11)).await?;
        self.wait(
            "reply PCM tail",
            |world| world.satellite.observed.borrow().played >= start_bytes + 8_640,
            0,
        )
        .await
    }

    pub async fn played(&mut self, index: usize, text: &str) -> Result<(), String> {
        let played = self.count("reply.played");
        self.transcript("reply-done", "assistant", text, true).await?;
        self.wait(
            "device and bridge Played",
            |world| {
                world.count("reply.played") == played + 1
                    && world.satellite.observed.borrow().replies_played() == index + 1
            },
            20,
        )
        .await
    }

    pub async fn transcript(&self, tag: &'static str, role: &str, text: &str, done: bool) -> Result<(), String> {
        let kind = if role == "user" {
            "input_transcript.added"
        } else {
            "output_transcript.added"
        };
        let event = if done {
            json!({"type":"turn.done","turn":{"role":role,"transcript":text}})
        } else {
            json!({"type":kind,"item":{"text":text}})
        };
        self.provider.control(Control::Events(tag, vec![event])).await
    }

    pub fn count(&self, event: &str) -> usize {
        self.trace
            .records
            .iter()
            .filter(|row| row["run_id"] == self.run_id && row["event"] == event)
            .count()
    }

    pub fn epoch(&self) -> u64 {
        self.clock["epoch_ms"].as_u64().unwrap_or(0)
    }

    pub async fn ready(&mut self) -> Result<(), String> {
        self.screen.expect(&json!({"state":"speaking"})).await?;
        self.screen.next().await?;
        self.screen
            .expect(&json!({"state":"ready","message":"Echo Live ready"}))
            .await
    }

    pub async fn step(&mut self, milliseconds: u64) -> Result<(), String> {
        collect_playout_for(&mut self.bridge, &mut self.clock, milliseconds, self.run_id).await?;
        self.trace = read_trace(&self.root.0)?;
        Ok(())
    }

    pub async fn wait(&mut self, phase: &str, done: impl Fn(&Self) -> bool, milliseconds: u64) -> Result<(), String> {
        let expires = Instant::now() + Duration::from_secs(8);
        while !done(self) {
            if Instant::now() >= expires {
                let events: Vec<_> = self.trace.records.iter().map(|row| &row["event"]).collect();
                return Err(format!("duplex: {phase} stalled; trace events={events:?}"));
            }
            self.provider
                .tick()
                .await
                .map_err(|error| format!("duplex: {phase}: {error}"))?;
            self.step(milliseconds).await?;
        }
        Ok(())
    }

    pub async fn close(mut self) -> Result<(Value, Trace), String> {
        let quiet = self.provider.quiesce().await;
        let bridge = self.bridge.close().await;
        let satellite = self.satellite.close().await;
        let provider = self.provider.close().await;
        let snapshot = bridge.and(satellite).and(provider)?;
        quiet?;
        Ok((snapshot, read_trace(&self.root.0)?))
    }
}

fn read_trace(root: &Path) -> Result<Trace, String> {
    let bytes = std::fs::read(root.join("bridge.private.jsonl")).map_err(|_| "duplex: missing bridge trace")?;
    let mut trace = Trace::default();
    trace.append(&bytes)?;
    Ok(trace)
}
