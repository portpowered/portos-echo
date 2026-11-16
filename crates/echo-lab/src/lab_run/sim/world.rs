use std::path::{Path, PathBuf};
use std::time::Duration;

use echo_live::fake::{FakeLive, FakeResponse, FakeScript};
use echo_spoof_mcp::{Options as SpoofOptions, Spoof};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::process::Command;

use super::{
    process::{self, Process},
    trace::Trace,
};

#[derive(Default)]
pub struct World {
    pub m2: Option<(String, String)>,
    pub host: Option<Process>,
    pub bridge: Option<Process>,
    spoof: Option<Spoof>,
    live: Option<FakeLive>,
    pub trace: Trace,
    trace_path: PathBuf,
    pub ui_path: PathBuf,
    pub screen: Option<super::production::Screen>,
    pub(super) offset: u64,
    pub generation: u64,
    pub history: Vec<Trace>,
    pub restarts: Vec<Value>,
    pub initial_ready: Option<u64>,
    pub mute_blocked: bool,
    pub address: Option<std::net::SocketAddr>,
    pub run: Option<String>,
    pub endpoints: Vec<std::net::SocketAddr>,
}

impl World {
    pub async fn start(&mut self, python: &str, bridge: &Path, root: &Path) -> Result<(), String> {
        self.start_with(
            python,
            bridge,
            root,
            FakeLive::bind(FakeScript {
                events: if super::production::selected(bridge) {
                    vec![json!({"type":"response.audio.done"}).to_string()]
                } else {
                    vec![]
                },
                response: FakeResponse::Answer,
            }),
        )
        .await
    }

    pub(super) async fn start_with(
        &mut self,
        python: &str,
        bridge: &Path,
        root: &Path,
        live: impl std::future::Future<Output = Result<FakeLive, echo_live::call::CallError>>,
    ) -> Result<(), String> {
        self.start_owned(&mut process::host(python), bridge, root, live, process::connect)
            .await
    }

    async fn start_owned<F: std::future::Future<Output = bool>>(
        &mut self,
        host_command: &mut Command,
        bridge: &Path,
        root: &Path,
        live: impl std::future::Future<Output = Result<FakeLive, echo_live::call::CallError>>,
        probe: impl FnMut(std::net::SocketAddr) -> F,
    ) -> Result<(), String> {
        self.trace_path = root.join("bridge.private.jsonl");
        self.ui_path = root.join("ui.private.json");
        let deadline = process::readiness_deadline();
        self.host = Some(Process::start(host_command)?);
        let host = self.host.as_mut().ok_or("launch: missing host")?;
        let address = process::admit_host(&mut self.endpoints, host.expect("host.ready"), deadline, probe).await?;
        self.spoof = Some(
            Spoof::start(SpoofOptions::default())
                .await
                .map_err(|_| "launch: spoof startup failed")?,
        );
        process::admit(
            &mut self.endpoints,
            Self::endpoint(self.spoof.as_ref().ok_or("launch: missing spoof")?.url())?,
            process::readiness_deadline(),
            process::connect,
        )
        .await?;
        self.live = Some(live.await.map_err(|_| "launch: fake Live startup failed")?);
        process::admit(
            &mut self.endpoints,
            Self::endpoint(&self.live.as_ref().ok_or("launch: missing fake Live")?.base_url())?,
            process::readiness_deadline(),
            process::connect,
        )
        .await?;
        self.start_bridge(bridge, root, address).await
    }

    pub(super) async fn start_bridge(
        &mut self,
        bridge: &Path,
        root: &Path,
        address: std::net::SocketAddr,
    ) -> Result<(), String> {
        self.generation += 1;
        self.address = Some(address);
        let mut command = Command::new(bridge);
        command
            .env("ECHO_LAB_WYOMING_ADDR", address.to_string())
            .env(
                "ECHO_LAB_MCP_URL",
                self.spoof.as_ref().ok_or("launch: missing spoof")?.url(),
            )
            .env(
                "ECHO_LAB_LIVE_URL",
                self.live.as_ref().ok_or("launch: missing fake Live")?.base_url(),
            )
            .env("ECHO_LAB_TRACE_PATH", &self.trace_path)
            .env("ECHO_LAB_UI_STATE_PATH", &self.ui_path);
        if super::production::selected(bridge) {
            super::production::configure(
                &mut command,
                root,
                address,
                &self.live.as_ref().ok_or("launch: missing fake Live")?.base_url(),
                self.spoof.as_ref().ok_or("launch: missing spoof")?.url(),
            )?;
            if let Some((case, auth)) = &self.m2 {
                super::m2::auth::configure(&mut command, bridge, root, case, auth).await?;
            }
        }
        self.bridge = Some(Process::start(&mut command)?);
        self.bridge
            .as_mut()
            .ok_or("launch: missing bridge")?
            .expect("bridge.ready")
            .await?;
        self.control(&json!({"action":"registered"})).await?;
        if super::production::selected(bridge) {
            self.connect_screen().await?;
        }
        Ok(())
    }

    async fn connect_screen(&mut self) -> Result<(), String> {
        self.observe().await?;
        for event in ["bridge.started", "screen.listening"] {
            let field = if event == "bridge.started" {
                "health_addr"
            } else {
                "address"
            };
            let address = self
                .trace
                .records
                .iter()
                .find(|r| r["event"] == event)
                .and_then(|r| r[field].as_str())
                .ok_or("launch: missing production endpoint")?
                .parse()
                .map_err(|_| "launch: invalid production endpoint")?;
            self.endpoints.push(address);
        }
        self.screen = Some(
            super::production::Screen::connect(
                *self.endpoints.last().ok_or("launch: missing screen")?,
                self.generation == 1,
            )
            .await?,
        );
        if self.generation == 1 {
            self.initial_ready = self.screen.as_ref().map(super::production::Screen::sequence);
        }
        Ok(())
    }

    fn endpoint(url: &str) -> Result<std::net::SocketAddr, String> {
        let address: std::net::SocketAddr = url
            .strip_prefix("http://")
            .and_then(|s| s.split('/').next())
            .ok_or("launch: invalid fake endpoint")?
            .parse()
            .map_err(|_| "launch: invalid fake endpoint")?;
        if address.ip() != std::net::Ipv4Addr::LOCALHOST || address.port() == 0 {
            return Err("launch: fake must bind 127.0.0.1".into());
        }
        Ok(address)
    }

    pub fn records(&self, generation: u64) -> &[Value] {
        if generation == self.generation {
            &self.trace.records
        } else {
            self.history
                .get(usize::try_from(generation.saturating_sub(1)).unwrap_or(usize::MAX))
                .map_or(&[], |t| t.records.as_slice())
        }
    }

    pub fn live_observer(&self) -> Result<echo_live::fake::FakeObserver, String> {
        self.live
            .as_ref()
            .map(echo_live::fake::FakeLive::observer)
            .ok_or("m2: missing provider".into())
    }

    pub async fn control(&mut self, value: &Value) -> Result<(), String> {
        let host = self.host.as_mut().ok_or("host: not started")?;
        host.send(value).await?;
        host.expect("host.ack").await?;
        Ok(())
    }

    pub async fn complete_run(&mut self) -> Result<(), String> {
        let value = self
            .bridge
            .as_mut()
            .ok_or("bridge: not started")?
            .expect("bridge.run_completed")
            .await?;
        let id = value["run_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("bridge: missing completed run ID")?;
        self.run = Some(id.into());
        self.observe().await
    }

    pub async fn observe(&mut self) -> Result<(), String> {
        loop {
            self.refresh().await?;
            if !self.trace.partial() {
                return Ok(());
            }
            if self.bridge.as_mut().ok_or("trace: bridge not started")?.exited()? {
                return self.trace.finish();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    pub async fn refresh(&mut self) -> Result<(), String> {
        let mut file = tokio::fs::File::open(&self.trace_path)
            .await
            .map_err(|_| "trace: missing bridge trace")?;
        if file.metadata().await.map_err(|_| "trace: cannot stat trace")?.len() < self.offset {
            return Err("trace: bridge truncated its trace".into());
        }
        file.seek(std::io::SeekFrom::Start(self.offset))
            .await
            .map_err(|_| "trace: cannot seek")?;
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "trace: cannot read")?;
        self.offset += u64::try_from(bytes.len()).map_err(|_| "trace: size overflow")?;
        self.trace.append(&bytes)?;
        if self.run.is_none() {
            self.run = self
                .trace
                .records
                .iter()
                .rev()
                .find(|r| r["event"] == "wyoming.run_ended")
                .and_then(|r| r["run_id"].as_str())
                .map(str::to_owned);
        }
        Ok(())
    }

    pub async fn close(&mut self) -> Result<(), String> {
        self.screen.take();
        let mut failures = self.close_bridge().await;
        if let Some(live) = self.live.take() {
            live.shutdown().await;
        }
        if let Some(spoof) = self.spoof.take()
            && spoof.shutdown().await.is_err()
        {
            failures.push("cleanup: spoof shutdown failed");
        }
        if let Some(host) = self.host.take()
            && !close_process(host).await
        {
            failures.push("cleanup: host failed to close");
        }
        if self.remove_private().await.is_err() {
            failures.push("cleanup: cannot remove private observation");
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    pub(super) async fn close_bridge(&mut self) -> Vec<&'static str> {
        let mut failures = Vec::new();
        if let Some(bridge) = self.bridge.take() {
            if !close_process(bridge).await {
                failures.push("cleanup: bridge failed to close");
            }
            if let Err(error) = self.refresh().await.and_then(|()| self.trace.finish()) {
                failures.push(if error.starts_with("trace: incomplete") {
                    "trace: incomplete final JSON line"
                } else {
                    "trace: invalid final trace"
                });
            }
        }
        failures
    }

    pub(super) async fn remove_private(&self) -> Result<(), String> {
        let mut failed = false;
        let private = self.trace_path.parent().map(|root| root.join("configured.private"));
        if let Some(private) = private.filter(|path| path.is_dir()) {
            failed |= tokio::fs::remove_dir_all(private).await.is_err();
        }
        for path in [&self.trace_path, &self.ui_path] {
            if path.is_file() && tokio::fs::remove_file(path).await.is_err() {
                failed = true;
            }
        }
        if failed {
            Err("cleanup: cannot remove private observation".into())
        } else {
            Ok(())
        }
    }
}

async fn close_process(mut process: Process) -> bool {
    process.close().await.is_ok()
}

#[cfg(test)]
mod tests;
