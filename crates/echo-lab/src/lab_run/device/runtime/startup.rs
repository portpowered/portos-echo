//! Select the production child only after explicit CLI admission.
use super::super::super::sim::{process::Process, production::Screen};
use super::{Runtime, Startup};
use echo_live::fake::{FakeLive, FakeResponse, FakeScript};
use serde_json::json;
use std::path::Path;

impl Runtime {
    pub async fn start(
        &mut self,
        bridge: Option<&Path>,
        config: &str,
        root: &Path,
        real: Option<(&Path, bool)>,
    ) -> Result<(), String> {
        self.startup = Some(Startup::new("config_stage"));
        self.started = true;
        self.began = Some(std::time::Instant::now());
        self.trace_path = root.join("bridge.private.jsonl");
        self.start_spoof().await?;
        if self.remote.is_some() {
            self.stage_remote(config, root)?;
            self.generation = 1;
            self.phase("bridge_spawn")?;
            return self.launch_remote().await;
        }
        self.real_config = real.map(|(path, overlay)| {
            if overlay {
                root.join("configured.private/bridge.toml")
            } else {
                path.to_owned()
            }
        });
        if self.real_config.is_some() {
            std::fs::create_dir(root.join("configured.private"))
                .map_err(|_| "device: cannot create private configuration")?;
            if real.is_some_and(|(_, overlay)| overlay) {
                super::super::config::write_private(&root.join("configured.private/bridge.toml"), config)?;
            }
        } else {
            self.start_fake(config, root).await?;
        }
        self.generation = 1;
        self.phase("bridge_spawn")?;
        self.launch(bridge.ok_or("device: missing bridge")?, root).await
    }

    async fn start_fake(&mut self, config: &str, root: &Path) -> Result<(), String> {
        super::super::config::stage(root, config)?;
        self.phase("fake_live_bind")?;
        self.live = Some(
            FakeLive::bind(FakeScript {
                events: vec![json!({"type":"response.audio.done"}).to_string()],
                response: FakeResponse::Answer,
            })
            .await
            .map_err(|_| "device: fake Live startup failed")?,
        );
        Ok(())
    }

    pub(in crate::lab_run::device) async fn launch(&mut self, bridge: &Path, root: &Path) -> Result<(), String> {
        let mut command = self.command(bridge, root)?;
        self.bridge = Some(Process::start(&mut command)?);
        self.phase("bridge_ready")?;
        self.child()?.expect("bridge.ready").await?;
        self.phase("wyoming_connect")?;
        self.observe()?;
        if !self.trace.records.iter().any(|r| r["event"] == "wyoming.connected") {
            return Err("device: ready without fresh Wyoming connection".into());
        }
        self.phase("screen_endpoint")?;
        let address = self.screen_address()?;
        self.screen = Some(Screen::connect(address, true).await?);
        self.startup.as_ref().ok_or("device: missing startup")?.check()
    }

    pub(in crate::lab_run::device) fn screen_address(&self) -> Result<std::net::SocketAddr, String> {
        let address = self
            .trace
            .records
            .iter()
            .find(|r| r["event"] == "screen.listening")
            .and_then(|r| r["address"].as_str())
            .ok_or("device: missing screen endpoint")?
            .parse::<std::net::SocketAddr>()
            .map_err(|_| "device: invalid screen endpoint")?;
        if address.ip() != std::net::Ipv4Addr::LOCALHOST || address.port() == 0 {
            return Err("device: screen endpoint must be loopback".into());
        }
        Ok(address)
    }

    fn command(&self, bridge: &Path, root: &Path) -> Result<tokio::process::Command, String> {
        let mut command = tokio::process::Command::new(bridge);
        for name in [
            "ECHO_LAB_WYOMING_ADDR",
            "ECHO_LAB_LIVE_URL",
            "ECHO_LAB_TRACE_PATH",
            "ECHO_LAB_SESSION_IDLE_SECONDS",
            "ECHO_LAB_UI_STATE_PATH",
            "ECHO_LAB_MCP_URL",
        ] {
            command.env_remove(name);
        }
        if let Some(path) = &self.real_config {
            command
                .arg("--config")
                .arg(path)
                .args(["--health-listen", "127.0.0.1:0"]);
        } else {
            let origin = self.live.as_ref().ok_or("device: missing fake Live")?.base_url();
            command.current_dir(root.join("configured.private")).args([
                "--config",
                "bridge.toml",
                "--health-listen",
                "127.0.0.1:0",
                "--live-loopback",
                origin.trim_end_matches('/'),
                "--model-loopback",
                origin.trim_end_matches('/'),
            ]);
        }
        command.arg("--trace").arg(&self.trace_path);
        Ok(command)
    }
}
