use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use echo_contracts::secret::Secret;
use echo_lab::lab_mcp::{Approval, Fixture, LinkCounts, Observations};
use echo_live::fake::{FakeLive, FakeResponse, FakeScript};
use echo_spoof_mcp::{Options as SpoofOptions, Spoof};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use super::{
    super::{
        process::{self, Process},
        trace::Trace,
    },
    mcp_wire,
};
use crate::lab_run::run_options::Options;

type Pending = JoinHandle<std::io::Result<std::process::Output>>;
const EPOCH: u64 = 1_800_000_000;

#[derive(Default)]
pub struct Session {
    host: Option<Process>,
    bridge: Option<Process>,
    fixture: Option<Fixture>,
    spoof: Option<Spoof>,
    live: Option<FakeLive>,
    pending: Option<Pending>,
    stream: Option<mcp_wire::Screen>,
    pub screen: Value,
    pub trace: Trace,
    observations: Observations,
    offset: usize,
    directory: PathBuf,
    binary: PathBuf,
    measured: Option<LinkCounts>,
    elapsed_ms: u64,
    pub endpoints: Vec<SocketAddr>,
}

impl Session {
    pub fn started(&self) -> bool {
        !self.directory.as_os_str().is_empty()
    }

    pub async fn start(&mut self, options: &Options, root: &Path) -> Result<(), String> {
        self.binary = options.bridge.clone().ok_or("M5: missing production bridge")?;
        let directory = root.join("mcp.private");
        std::fs::create_dir(&directory).map_err(|_| "M5: cannot own private state directory")?;
        self.directory = directory;
        self.host = Some(Process::start(&mut process::host(&options.python))?);
        let host = self.host.as_mut().ok_or("M5: missing host")?;
        let address = process::admit_host(
            &mut self.endpoints,
            host.expect("host.ready"),
            process::readiness_deadline(),
            process::connect,
        )
        .await?;
        let origin = self.start_peers(address).await?;
        self.launch(&origin).await
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
        self.bridge = Some(Process::start(&mut command)?);
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
        let origin = self
            .live
            .as_ref()
            .ok_or("M5: missing Live fixture")?
            .base_url()
            .trim_end_matches('/')
            .to_owned();
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

    fn grant(&self) -> Result<Secret<Value>, String> {
        let bytes = Secret::new(super::super::observers::bytes(
            &self.directory.join("portos/grant.json"),
            65536,
            "M5 private grant",
        )?);
        serde_json::from_slice(bytes.expose())
            .map(Secret::new)
            .map_err(|_| "M5: invalid private grant".into())
    }

    async fn advance_bridge(&mut self, milliseconds: u64) -> Result<(), String> {
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

    async fn start_peers(&mut self, address: SocketAddr) -> Result<String, String> {
        self.fixture = Some(
            Fixture::bind(EPOCH)
                .await
                .map_err(|_| "M5: authorization startup failed")?,
        );
        let fixture = self.fixture.as_ref().ok_or("M5: missing authorization fixture")?;
        self.spoof = Some(
            Spoof::start_authorized(SpoofOptions::default(), Some(fixture.validator()))
                .await
                .map_err(|_| "M5: resource startup failed")?,
        );
        let spoof = self.spoof.as_ref().ok_or("M5: missing resource")?;
        fixture
            .set_resource(spoof.url())
            .map_err(|_| "M5: resource binding failed")?;
        self.endpoints.push(endpoint(fixture.base())?);
        self.endpoints.push(endpoint(spoof.url())?);
        self.live = Some(
            FakeLive::bind(FakeScript {
                events: vec![],
                response: FakeResponse::Answer,
            })
            .await
            .map_err(|_| "M5: Live startup failed")?,
        );
        let origin = self.live.as_ref().ok_or("M5: missing Live fixture")?.base_url();
        self.endpoints.push(endpoint(&origin)?);
        mcp_wire::configure(&self.directory, address, spoof.url())?;
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

    fn invoke(&self, verb: &str) -> Pending {
        let mut command = mcp_wire::command(&self.binary, &self.directory);
        command.args([verb, "--config", "bridge.toml"]);
        tokio::spawn(async move { command.output().await })
    }

    pub async fn link(&mut self) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("M5: link already pending".into());
        }
        self.pending = Some(self.invoke("link"));
        self.receive_screen().await?;
        let fixture = self.fixture.as_ref().ok_or("M5: missing authorization fixture")?;
        if self.screen != mcp_wire::link_code(EPOCH, fixture.base()) {
            return Err("M5: link code mismatch".into());
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
        mcp_wire::completion(&output, "MCP linked\n")?;
        self.receive_screen().await?;
        if self.screen != json!({"state":"ready","message":"Echo Live ready"}) {
            return Err("M5: approved screen mismatch".into());
        }
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

    async fn receive_screen(&mut self) -> Result<(), String> {
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
            if let Err(error) = bridge.close().await {
                failures.push(error);
            }
            if let Err(error) = self.observe().and_then(|()| self.trace.finish()) {
                failures.push(error);
            }
        }
        failures
    }

    async fn close_bridge(&mut self) -> Vec<String> {
        let mut failures = self.stop_bridge().await;
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
        if let Some(live) = self.live.take() {
            live.shutdown().await;
        }
        if let Some(spoof) = self.spoof.take()
            && spoof.shutdown().await.is_err()
        {
            failures.push("M5: resource cleanup failed".into());
        }
        if let Some(fixture) = self.fixture.take()
            && fixture.shutdown().await.is_err()
        {
            failures.push("M5: authorization cleanup failed".into());
        }
        if let Some(mut host) = self.host.take()
            && let Err(error) = host.close().await
        {
            failures.push(error);
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

fn endpoint(value: &str) -> Result<SocketAddr, String> {
    value
        .strip_prefix("http://")
        .and_then(|value| value.split('/').next())
        .ok_or_else(|| "M5: invalid local endpoint".into())
        .and_then(|value| value.parse().map_err(|_| "M5: invalid local endpoint".into()))
}
