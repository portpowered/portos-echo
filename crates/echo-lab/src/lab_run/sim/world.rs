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
    pub host: Option<Process>,
    pub bridge: Option<Process>,
    spoof: Option<Spoof>,
    live: Option<FakeLive>,
    pub trace: Trace,
    trace_path: PathBuf,
    pub ui_path: PathBuf,
    offset: u64,
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
                events: vec![],
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
        self.trace_path = root.join("bridge.private.jsonl");
        self.ui_path = root.join("ui.private.json");
        self.host = Some(Process::start(&mut process::host(python))?);
        let host = self.host.as_mut().ok_or("launch: missing host")?;
        let value = host.expect("host.ready").await?;
        let address = value["address"].as_str().ok_or("launch: missing host address")?;
        let address: std::net::SocketAddr = address.parse().map_err(|_| "launch: invalid host address")?;
        if address.ip() != std::net::Ipv4Addr::LOCALHOST || address.port() == 0 {
            return Err("launch: host must target 127.0.0.1".into());
        }
        self.endpoints.push(address);
        self.spoof = Some(
            Spoof::start(SpoofOptions::default())
                .await
                .map_err(|_| "launch: spoof startup failed")?,
        );
        self.endpoints.push(Self::endpoint(
            self.spoof.as_ref().ok_or("launch: missing spoof")?.url(),
        )?);
        self.live = Some(live.await.map_err(|_| "launch: fake Live startup failed")?);
        self.endpoints.push(Self::endpoint(
            &self.live.as_ref().ok_or("launch: missing fake Live")?.base_url(),
        )?);
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
        self.bridge = Some(Process::start(&mut command)?);
        self.bridge
            .as_mut()
            .ok_or("launch: missing bridge")?
            .expect("bridge.ready")
            .await?;
        self.control(&json!({"action":"registered"})).await
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

    async fn close_bridge(&mut self) -> Vec<&'static str> {
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

    async fn remove_private(&self) -> Result<(), String> {
        let mut failed = false;
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
    if tokio::time::timeout(Duration::from_secs(5), process.close()).await == Ok(Ok(())) {
        return true;
    }
    process.terminate().await;
    false
}
