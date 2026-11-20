use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use echo_contracts::secret::Secret;
use echo_lab::lab_mcp::{Approval, Fixture, LinkCounts, Observations};
use echo_spoof_mcp::{Options as SpoofOptions, Spoof};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use super::{
    super::{
        process::{self, Process},
        trace::Trace,
    },
    mcp_lights::Journey,
    mcp_wire::{self, endpoint},
};
use crate::lab_run::run_options::Options;

type Pending = JoinHandle<std::io::Result<std::process::Output>>;
pub(super) const EPOCH: u64 = 1_800_000_000;

#[derive(Default)]
pub struct Session {
    pub(super) journey: Journey,
    pub(super) host: Option<Process>,
    pub(super) bridge: Option<Process>,
    pub(super) fixture: Option<Fixture>,
    pub(super) spoof: Option<Spoof>,
    pub(super) live: Option<super::mcp_peers::Peers>,
    pub(super) pending: Option<Pending>,
    pub(super) stream: Option<mcp_wire::Screen>,
    pub screen: Value,
    pub trace: Trace,
    pub(super) observations: Observations,
    offset: usize,
    pub(super) directory: PathBuf,
    binary: PathBuf,
    measured: Option<LinkCounts>,
    pub(super) elapsed_ms: u64,
    pub(super) initial: Option<Secret<Value>>,
    pub(super) replayed: bool,
    pub endpoints: Vec<SocketAddr>,
    #[cfg(test)]
    released: Vec<&'static str>,
    root: Option<super::super::m2::codex::config::Root>,
}

impl Session {
    pub(super) fn new(journey: Journey) -> Self {
        Self {
            journey,
            ..Self::default()
        }
    }

    pub fn started(&self) -> bool {
        !self.directory.as_os_str().is_empty()
    }

    pub async fn start(&mut self, options: &Options, _root: &Path) -> Result<(), String> {
        self.prepare(options)?;
        self.host = Some(Process::start_observed(&mut process::host(&options.python))?);
        let host = self.host.as_mut().ok_or("M5: missing host")?;
        let address = process::admit_host(
            &mut self.endpoints,
            host.expect("host.ready"),
            process::readiness_deadline(),
            process::connect,
        )
        .await?;
        let origin = self.start_peers(address, &options.python).await?;
        self.launch(&origin).await
    }

    fn prepare(&mut self, options: &Options) -> Result<(), String> {
        self.binary = options.bridge.clone().ok_or("M5: missing production bridge")?;
        self.root = Some(super::super::m2::codex::config::Root::new()?);
        let directory = self.root.as_ref().ok_or("M5: missing private root")?.0.clone();
        self.directory = directory;
        Ok(())
    }

    async fn launch(&mut self, origin: &str) -> Result<(), String> {
        let mut command = mcp_wire::command(&self.binary, &self.directory);
        command.args([
            "--config",
            "bridge.toml",
            "--trace",
            "trace",
            "--health-listen",
            "127.0.0.1:0",
            "--live-loopback",
            origin,
            "--model-loopback",
            origin,
            "--sim-clock",
            "--sim-epoch",
            &EPOCH.to_string(),
        ]);
        self.bridge = Some(Process::start_observed(&mut command)?);
        self.bridge
            .as_mut()
            .ok_or("M5: missing bridge")?
            .expect("bridge.ready")
            .await?;
        self.host
            .as_mut()
            .ok_or("M5: missing host")?
            .send(&json!({"action":"registered"}))
            .await?;
        self.host.as_mut().ok_or("M5: missing host")?.expect("host.ack").await?;
        self.observe()?;
        self.configure_control()?;
        let screen = self.address("screen.listening")?;
        self.stream = Some(mcp_wire::screen(&screen).await?);
        self.receive_screen().await?;
        if self.screen != json!({"state":"ready","message":"Echo Live ready"}) {
            return Err("M5: initial screen mismatch".into());
        }
        Ok(())
    }

    pub async fn restart(&mut self) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("M5: restart refused while link is pending".into());
        }
        let stored = self.grant()?;
        if stored.expose()["grant"].is_null() {
            return Err("M5: restart requires a stored grant".into());
        }
        let before = self
            .fixture
            .as_ref()
            .ok_or("M5: missing authorization fixture")?
            .counts()
            .map_err(|_| "M5: authorization observation failed")?;
        self.stream.take();
        let failures = self.stop_bridge().await;
        if !failures.is_empty() {
            return Err(failures.join("; "));
        }
        self.trace.restarted()?;
        self.observations.restarted();
        let origin = self.live.as_ref().ok_or("M5: missing Live fixture")?.origin.clone();
        self.launch(&origin).await?;
        // Reuse the recorded epoch, restoring elapsed time without advancing AS
        // UTC a second time. No AuthClient/store writer exists in the driver.
        if self.elapsed_ms != 0 {
            self.advance_bridge(self.elapsed_ms).await?;
        }
        self.observe()?;
        let after = self
            .fixture
            .as_ref()
            .ok_or("M5: missing authorization fixture")?
            .counts()
            .map_err(|_| "M5: authorization observation failed")?;
        if self.grant()?.expose() != stored.expose() || after != before {
            return Err("M5: restart changed the stored grant or authorization counters".into());
        }
        Ok(())
    }

    pub(super) fn grant(&self) -> Result<Secret<Value>, String> {
        let bytes = Secret::new(super::super::observers::bytes(
            &self.directory.join("portos/grant.json"),
            65536,
            "M5 private grant",
        )?);
        serde_json::from_slice(bytes.expose())
            .map(Secret::new)
            .map_err(|_| "M5: invalid private grant".into())
    }

    pub(super) async fn advance_bridge(&mut self, milliseconds: u64) -> Result<(), String> {
        let bridge = self.bridge.as_mut().ok_or("M5: missing bridge")?;
        bridge
            .send(&json!({"action":"advance_clock", "milliseconds":milliseconds}))
            .await?;
        let ack = bridge.expect("bridge.clock_advanced").await?;
        if ack["milliseconds"] != milliseconds {
            return Err("M5: clock acknowledgment mismatch".into());
        }
        Ok(())
    }

    async fn start_peers(&mut self, address: SocketAddr, python: &str) -> Result<String, String> {
        self.fixture = Some(
            Fixture::bind(EPOCH)
                .await
                .map_err(|_| "M5: authorization startup failed")?,
        );
        let fixture = self.fixture.as_ref().ok_or("M5: missing authorization fixture")?;
        self.endpoints.push(endpoint(fixture.base())?);
        let resource = if self.journey == Journey::Lifecycle {
            let spoof = Spoof::start_authorized(SpoofOptions::default(), Some(fixture.validator()))
                .await
                .map_err(|_| "M5: resource startup failed")?;
            let url = spoof.url().to_owned();
            fixture.set_resource(&url).map_err(|_| "M5: resource binding failed")?;
            self.endpoints.push(endpoint(&url)?);
            self.spoof = Some(spoof);
            url
        } else {
            format!("{}/mcp", fixture.base())
        };
        self.live = Some(super::mcp_peers::Peers::bind(python, self.journey).await?);
        let origin = self.live.as_ref().ok_or("M5: missing Live fixture")?.origin.clone();
        self.endpoints.push(endpoint(&origin)?);
        mcp_wire::configure(&self.directory, address, &resource, self.journey)?;
        Ok(origin.trim_end_matches('/').into())
    }

    fn address(&self, event: &str) -> Result<String, String> {
        let address = self
            .trace
            .records
            .iter()
            .rev()
            .find(|record| record["event"] == event)
            .and_then(|record| record["address"].as_str())
            .ok_or("M5: missing listening observation")?;
        let parsed: SocketAddr = address.parse().map_err(|_| "M5: invalid listening observation")?;
        if parsed.ip() != std::net::Ipv4Addr::LOCALHOST || parsed.port() == 0 {
            return Err("M5: listener must be loopback".into());
        }
        Ok(address.into())
    }

    fn configure_control(&mut self) -> Result<(), String> {
        let control = self.address("mcp.operator.listening")?;
        for event in ["mcp.operator.listening", "screen.listening"] {
            self.endpoints
                .push(self.address(event)?.parse().map_err(|_| "M5: invalid listener")?);
        }
        let health = self
            .trace
            .records
            .iter()
            .find(|record| record["event"] == "bridge.started")
            .and_then(|record| record["health_addr"].as_str())
            .ok_or("M5: missing health address")?;
        self.endpoints.push(endpoint(&format!("http://{health}"))?);
        let path = self.directory.join("bridge.toml");
        let text = std::fs::read_to_string(&path).map_err(|_| "M5: cannot read private config")?;
        std::fs::write(
            path,
            text.replace(
                "operator_listen = '127.0.0.1:0'",
                &format!("operator_listen = '{control}'"),
            ),
        )
        .map_err(|_| "M5: cannot bind operator config".into())
    }

    pub async fn link(&mut self) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("M5: link already pending".into());
        }
        let (pending, guidance) = mcp_wire::link_command(&self.binary, &self.directory);
        self.pending = Some(pending);
        let guidance = guidance.await.map_err(|_| "M5: missing pre-approval stdout")?;
        self.receive_screen().await?;
        let fixture = self.fixture.as_ref().ok_or("M5: missing authorization fixture")?;
        if self.screen != mcp_wire::link_code(EPOCH, fixture.base()) {
            return Err("M5: link code mismatch".into());
        }
        if guidance != format!("Verification URL: {}/approve\nUser code: TEST-CODE\n", fixture.base()).as_bytes()
            || !self.grant()?.expose()["grant"].is_null()
        {
            return Err("M5: pre-approval guidance/grant mismatch".into());
        }
        Ok(())
    }

    pub async fn approve(&mut self) -> Result<(), String> {
        if self.pending.is_none() {
            return Err("M5: approval requires a pending link".into());
        }
        let fixture = self.fixture.as_ref().ok_or("M5: missing authorization fixture")?;
        fixture
            .approve(Approval::Approved)
            .map_err(|_| "M5: cannot approve grant")?;
        fixture.advance(5).map_err(|_| "M5: cannot advance fixture clock")?;
        self.advance_bridge(5000).await?;
        self.elapsed_ms += 5000;
        let output = self
            .pending
            .as_mut()
            .ok_or("M5: missing pending link")?
            .await
            .map_err(|_| "M5: link task failed")?
            .map_err(|_| "M5: link command failed")?;
        self.pending.take();
        let issuer = self.fixture.as_ref().ok_or("M5: missing authorization fixture")?.base();
        mcp_wire::completion(
            &output,
            &format!("Verification URL: {issuer}/approve\nUser code: TEST-CODE\nMCP linked\n"),
        )?;
        self.receive_screen().await?;
        if self.screen != json!({"state":"ready","message":"Echo Live ready"}) {
            return Err("M5: approved screen mismatch".into());
        }
        self.initial = Some(self.grant()?);
        self.replayed = false;
        Ok(())
    }

    pub fn admit_replay(&self) -> Result<(), String> {
        if self.pending.is_some() || self.replayed {
            return Err("M5: replay requires a live rotated grant".into());
        }
        let initial = self
            .initial
            .as_ref()
            .ok_or("M5: replay requires a live rotated grant")?;
        let current = self.grant()?;
        if current.expose()["grant"].is_null()
            || current.expose()["grant"]["refresh_token"] == initial.expose()["grant"]["refresh_token"]
            || current.expose()["private_key_jwk"] != initial.expose()["private_key_jwk"]
        {
            return Err("M5: replay requires a live rotated grant".into());
        }
        Ok(())
    }

    pub async fn replay(&mut self) -> Result<(), String> {
        self.admit_replay()?;
        let current = self.grant()?;
        let fixture = self.fixture.as_ref().ok_or("M5: missing authorization fixture")?;
        let before = fixture.counts().map_err(|_| "M5: authorization observation failed")?;
        let token = current.expose()["grant"]["access_token"]
            .as_str()
            .ok_or("M5: missing access token")?;
        if !fixture.validator().accepts(token) {
            return Err("M5: replay requires a live rotated grant".into());
        }
        super::mcp_replay::reject(
            self.initial.as_ref().ok_or("M5: missing replay record")?,
            EPOCH + self.elapsed_ms / 1000,
            before.links,
        )
        .await?;
        let mut expected = before;
        expected.replay_rejected += 1;
        if fixture.counts().map_err(|_| "M5: authorization observation failed")? != expected
            || fixture.validator().accepts(token)
            || self.grant()?.expose() != current.expose()
        {
            return Err("M5: replay did not revoke the unchanged stored grant".into());
        }
        self.replayed = true;
        Ok(())
    }

    pub async fn operator(&mut self, verb: &str, expected: &str) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("M5: link is pending".into());
        }
        let mut command = mcp_wire::command(&self.binary, &self.directory);
        command.args([verb, "--config", "bridge.toml"]);
        let output = command.output().await.map_err(|_| "M5: operator command failed")?;
        mcp_wire::completion(&output, expected)?;
        if verb == "unlink" {
            self.receive_screen().await?;
        }
        Ok(())
    }

    pub(super) async fn receive_screen(&mut self) -> Result<(), String> {
        self.screen = mcp_wire::state(self.stream.as_mut().ok_or("M5: missing paired screen")?).await?;
        let state = serde_json::from_value(self.screen.clone()).map_err(|_| "M5: invalid screen state")?;
        self.observations.screen(&state);
        Ok(())
    }

    pub fn observe(&mut self) -> Result<(), String> {
        let bytes = super::super::observers::bytes(&self.directory.join("trace"), 1024 * 1024, "M5 trace")?;
        if bytes.len() < self.offset {
            return Err("M5: truncated trace".into());
        }
        let previous = self.trace.records.len();
        self.trace.append(&bytes[self.offset..])?;
        self.offset = bytes.len();
        for record in &self.trace.records[previous..] {
            self.observations.trace(record)?;
        }
        Ok(())
    }

    pub fn measure(&self) -> Result<LinkCounts, String> {
        if let Some(measured) = self.measured {
            return Ok(measured);
        }
        let fixture = self.fixture.as_ref().ok_or("M5: missing authorization observations")?;
        let spoof = self.spoof.as_ref().ok_or("M5: missing resource observations")?;
        self.observations
            .measure(
                fixture.counts().map_err(|_| "M5: authorization observation failed")?,
                &spoof.snapshot().map_err(|_| "M5: resource observation failed")?,
            )
            .map_err(str::to_owned)
    }

    pub fn final_measurement(&self) -> Option<LinkCounts> {
        self.measured
    }

    async fn stop_bridge(&mut self) -> Vec<String> {
        let mut failures = Vec::new();
        if let Some(mut bridge) = self.bridge.take() {
            let closed = bridge.close().await;
            #[cfg(test)]
            self.record_release("bridge", &closed);
            failures.extend(
                closed
                    .err()
                    .map(|error| format!("M5 bridge: {error}; {}", bridge.exit_summary())),
            );
            if let Err(error) = self.observe().and_then(|()| self.trace.finish()) {
                failures.push(error);
            }
        }
        failures
    }

    async fn close_bridge(&mut self) -> Vec<String> {
        let mut failures = Vec::new();
        if let Some(live) = &self.live
            && let Err(error) = live.quiesce().await
        {
            failures.push(error);
        }
        failures.extend(self.stop_bridge().await);
        if self.fixture.is_some() && self.spoof.is_some() {
            match self.measure() {
                Ok(counts) => self.measured = Some(counts),
                Err(error) => failures.push(error),
            }
        }
        failures
    }

    pub async fn close(&mut self) -> Result<(), String> {
        let mut failures = Vec::new();
        if let Some(pending) = self.pending.take() {
            pending.abort();
            let _ = pending.await;
        }
        self.stream.take();
        failures.extend(self.close_bridge().await);
        self.initial.take();
        if let Some(live) = self.live.take() {
            let closed = live.shutdown().await;
            #[cfg(test)]
            self.record_release("provider", &closed);
            failures.extend(closed.err().map(|error| format!("M5 provider: {error}")));
        }
        if let Some(spoof) = self.spoof.take() {
            let closed = spoof.shutdown().await;
            #[cfg(test)]
            self.record_release("spoof", &closed);
            failures.extend(closed.err().map(|_| "M5: resource cleanup failed".into()));
        }
        if let Some(fixture) = self.fixture.take() {
            let closed = fixture.shutdown().await;
            #[cfg(test)]
            self.record_release("authorization", &closed);
            failures.extend(closed.err().map(|_| "M5: authorization cleanup failed".into()));
        }
        if let Some(mut host) = self.host.take() {
            let closed = host.close().await;
            #[cfg(test)]
            self.record_release("host", &closed);
            failures.extend(
                closed
                    .err()
                    .map(|error| format!("M5 host: {error}; {}", host.exit_summary())),
            );
        }
        if self.started() && std::fs::remove_dir_all(&self.directory).is_err() {
            failures.push("M5: private state cleanup failed".into());
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(pending) = &self.pending {
            pending.abort();
        }
    }
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
