//! One fresh production Muse journey, using the shared finite Wyoming observer.
use super::super::{
    m4::{
        model::Model,
        screen::Screen,
        wire::Relay,
        world::{self, fixture},
    },
    process::{self, Process},
    trace::Trace,
};
use super::{live::Live, muse::Muse, oracle};
use echo_live::fake::DialogueCall;
use echo_spoof_mcp::{Options, Spoof};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const SUCCESS: &str = "Muse test job completed";
pub const DEADLINE: &str = "Muse did not report a result in time";

#[derive(Default)]
pub struct World {
    pub muse: Muse,
    host: Option<Process>,
    bridge: Option<Process>,
    relay: Option<Relay>,
    live: Option<Live>,
    model: Option<Model>,
    spoof: Option<Spoof>,
    root: PathBuf,
    pub trace: Trace,
    pub speaking: Option<Value>,
    pub dialogue: Vec<DialogueCall>,
    pub completion: Value,
    pub playback: Value,
    pub audibility: Value,
    pub ui_states: Vec<Value>,
    pub model_requests: usize,
    pub tool_calls: usize,
}

#[cfg(test)]
mod tests;

impl World {
    pub async fn start(
        &mut self,
        python: &str,
        bridge: &Path,
        root: &Path,
        negative: bool,
        binaries: &Path,
    ) -> Result<(), String> {
        std::fs::create_dir(root).map_err(|_| "m7: cannot own private world")?;
        self.root = root.into();
        self.muse.start(python, &root.join("muse"), binaries).await?;
        self.host = Some(Process::start(process::host(python).arg("--finite"))?);
        let ready = self.host.as_mut().ok_or("m7: missing host")?.expect("host.ready");
        let address = process::admit_host(&mut vec![], ready, process::readiness_deadline(), process::connect).await?;
        self.relay = Some(Relay::bind_fixture(address, fixture()).await?);
        self.spoof = Some(
            Spoof::start(Options::default())
                .await
                .map_err(|_| "m7: spoof startup failed")?,
        );
        self.model = Some(Model::bind_controlled(false, async { Ok(()) }, None).await?);
        let text = if negative { DEADLINE } else { SUCCESS };
        self.live = Some(Live::bind(text).await?);
        self.configure()?;
        self.bridge = Some(Process::start(&mut world::command(
            root,
            bridge,
            self.live.as_ref().ok_or("m7: missing Live")?.base_url(),
            &self.model.as_ref().ok_or("m7: missing model")?.origin,
        ))?);
        self.bridge
            .as_mut()
            .ok_or("m7: missing bridge")?
            .expect("bridge.ready")
            .await?;
        self.control(json!({"action":"registered"})).await.map(|_| ())
    }

    fn configure(&self) -> Result<(), String> {
        world::configure(
            &self.root,
            self.relay.as_ref().ok_or("m7: missing relay")?.address.port(),
            self.spoof.as_ref().ok_or("m7: missing spoof")?,
        )?;
        let path = self.root.join("bridge.toml");
        let config = std::fs::read_to_string(&path).map_err(|_| "m7: missing config")?;
        let (head, tail) = config.split_once("[[mcp.server]]").ok_or("m7: missing MCP section")?;
        let (_, tail) = tail.split_once("[screen]").ok_or("m7: missing screen section")?;
        let config =
            format!("{head}[mcp]\nserver = []\n[screen]{tail}").replace("[agent]", "[agent]\nbackend = 'muse'");
        std::fs::write(
            path,
            format!(
                "{config}\n[muse]\nurl = '{}'\nca_file = '{}'\ncredential_file = '{}'\n",
                self.muse.url,
                self.muse.root.join("server.der").display(),
                self.muse.root.join("muse-credential").display()
            ),
        )
        .map_err(|_| "m7: cannot write selected configuration".into())
    }

    async fn control(&mut self, value: Value) -> Result<Value, String> {
        let host = self.host.as_mut().ok_or("m7: missing host")?;
        host.send(&value).await?;
        host.expect("host.ack").await
    }

    pub async fn speak(&mut self, negative: bool) -> Result<(), String> {
        let mut screen = Screen::connect(&self.root).await?;
        self.ui_states.push(screen.expect("ready").await?);
        self.control(json!({"action":"finite","pcm":fixture()})).await?;
        self.ui_states.push(screen.expect("listening").await?);
        self.ui_states.push(screen.expect("connected").await?);
        let id = self.progress().await?;
        self.muse.terminal(&id, negative).await?;
        let speaking = screen.expect("speaking").await?;
        self.ui_states.push(speaking.clone());
        self.speaking = Some(speaking);
        self.completion = self
            .bridge
            .as_mut()
            .ok_or("m7: missing bridge")?
            .expect("bridge.run_completed")
            .await?;
        self.ui_states.push(screen.expect("ready").await?);
        self.playback = self.control(json!({"action":"playback"})).await?;
        self.relay.as_ref().ok_or("m7: missing relay")?.timing().sample()?;
        self.audibility = self
            .relay
            .as_ref()
            .ok_or("m7: missing relay")?
            .timing()
            .audible_reply(&self.playback)?;
        let live = self.live.as_mut().ok_or("m7: missing Live")?;
        live.completed().await?;
        self.dialogue = live.calls();
        self.muse.collect()
    }

    async fn progress(&self) -> Result<String, String> {
        loop {
            let raw = std::fs::read(self.root.join("trace")).map_err(|_| "m7: missing trace")?;
            let mut trace = Trace::default();
            trace.append(&raw)?;
            if let Some(record) = trace.records.iter().find(|e| e["event"] == "muse.progress") {
                return record["task_id"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or("m7: missing progress ID".into());
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    pub async fn close(&mut self) -> Result<(), String> {
        let mut failures = vec![];
        self.close_bridge(&mut failures).await;
        self.close_peers(&mut failures).await;
        if let Err(error) = self.muse.close().await {
            failures.push(error);
        }
        if let Some(mut host) = self.host.take()
            && let Err(error) = host.close().await
        {
            failures.push(error);
        }
        if !self.root.as_os_str().is_empty()
            && let Err(error) = std::fs::remove_dir_all(&self.root)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            failures.push(format!("m7: private world removal failed: {}", error.kind()));
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    async fn close_bridge(&mut self, failures: &mut Vec<String>) {
        if let Some(mut bridge) = self.bridge.take() {
            if let Err(error) = bridge.close().await {
                failures.push(error);
            }
            let result = std::fs::read(self.root.join("trace"))
                .map_err(|_| "m7: missing final trace".into())
                .and_then(|raw| self.trace.append(&raw))
                .and_then(|()| self.trace.finish());
            if let Err(error) = result {
                failures.push(error);
            }
        }
        if let Some(model) = &self.model {
            self.model_requests = model.requests();
        }
        if let Some(spoof) = &self.spoof {
            match spoof.snapshot() {
                Ok(snapshot) => self.tool_calls = snapshot.calls.len(),
                Err(_) => failures.push("m7: missing client tool observations".into()),
            }
        }
    }

    async fn close_peers(&mut self, failures: &mut Vec<String>) {
        if let Some(relay) = self.relay.take()
            && let Err(error) = relay.close().await
        {
            failures.push(error);
        }
        if let Some(live) = self.live.take()
            && live.shutdown().await.is_err()
        {
            failures.push("m7: Live cleanup failed".into());
        }
        if let Some(model) = self.model.take()
            && let Err(error) = model.close().await
        {
            failures.push(error);
        }
        if let Some(spoof) = self.spoof.take()
            && spoof.shutdown().await.is_err()
        {
            failures.push("m7: spoof cleanup failed".into());
        }
    }

    pub fn observation(&self, negative: bool) -> Result<Value, String> {
        oracle::observe(self, negative)
    }
}
