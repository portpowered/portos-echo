//! One fresh production Muse journey, using the shared finite Wyoming observer.
use super::super::{
    m4::{
        codex::{self, provider::Provider},
        screen::Screen,
        wire::Relay,
        world::{self, fixture},
    },
    process::{self, Process},
    trace::Trace,
};
use super::{muse::Muse, oracle};
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
    provider: Option<Provider>,
    spoof: Option<Spoof>,
    root: PathBuf,
    pub trace: Trace,
    pub speaking: Option<Value>,
    pub dialogue: Value,
    pub completion: Value,
    pub playback: Value,
    pub audibility: Value,
    pub ui_states: Vec<Value>,
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
        let text = if negative { DEADLINE } else { SUCCESS };
        self.provider = Some(Provider::start_muse(python, text).await?);
        self.configure()?;
        let origin = &self.provider.as_ref().ok_or("m7: missing provider")?.origin;
        self.bridge = Some(Process::start(
            world::command(root, bridge, origin, origin).arg("--sim-clock"),
        )?);
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
        codex::configure(&self.root)?;
        let path = self.root.join("bridge.toml");
        let config = std::fs::read_to_string(&path).map_err(|_| "m7: missing config")?;
        let (head, tail) = config.split_once("[[mcp.server]]").ok_or("m7: missing MCP section")?;
        let (_, tail) = tail.split_once("[screen]").ok_or("m7: missing screen section")?;
        let config =
            format!("{head}[mcp]\nserver = []\n[screen]{tail}").replace("[voice]", "[voice]\nsession_max_seconds = 10");
        std::fs::write(
            path,
            format!(
                "{config}\n[trace]\npayloads = false\n[muse]\nurl = '{}'\nca_file = '{}'\ncredential_file = '{}'\n",
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
        self.speak_with(async |root, muse| muse.terminal(&progress(root).await?, negative).await)
            .await
    }

    async fn speak_with(
        &mut self,
        task: impl AsyncFnOnce(&Path, &mut Muse) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut screen = Screen::connect(&self.root).await?;
        self.ui_states.push(screen.expect("ready").await?);
        self.control(json!({"action":"finite","pcm":fixture()})).await?;
        let provider = self.provider.as_mut().ok_or("m7: missing provider")?;
        let bridge = self.bridge.as_mut().ok_or("m7: missing bridge")?;
        let relay = self.relay.as_ref().ok_or("m7: missing relay")?;
        let (connected, ready) = tokio::sync::oneshot::channel();
        let mut clock = Value::Null;
        let (completion, speaking, ()) = tokio::try_join!(
            codex::settle(bridge, provider, relay, ready, &mut clock),
            observe_screen(&mut screen, &mut self.ui_states, connected),
            task(&self.root, &mut self.muse),
        )?;
        self.completion = completion;
        self.speaking = Some(speaking);
        self.playback = self.control(json!({"action":"playback"})).await?;
        let timing = self.relay.as_ref().ok_or("m7: missing relay")?.timing();
        timing.sample()?;
        self.audibility = timing.audible_reply(&self.playback)?;
        self.muse.collect()
    }

    pub async fn close(&mut self) -> Result<(), String> {
        let mut failures = vec![];
        if let Some(provider) = &self.provider {
            failures.extend(provider.quiesce().await.err());
        }
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
        if let Some(provider) = self.provider.take() {
            match provider.close().await {
                Ok(snapshot) => self.dialogue = snapshot,
                Err(error) => failures.push(error),
            }
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

async fn observe_screen(
    screen: &mut Screen,
    states: &mut Vec<Value>,
    connected: tokio::sync::oneshot::Sender<()>,
) -> Result<Value, String> {
    for state in ["listening", "connected"] {
        states.push(screen.expect(state).await?);
    }
    connected.send(()).map_err(|()| "m7: connection observer abandoned")?;
    states.push(screen.expect("thinking").await?);
    let speaking = screen.expect("speaking").await?;
    states.push(speaking.clone());
    states.push(screen.expect("connected").await?);
    states.push(screen.expect("ready").await?);
    Ok(speaking)
}

async fn progress(root: &Path) -> Result<String, String> {
    let mut checks = tokio::time::interval(std::time::Duration::from_millis(5));
    loop {
        checks.tick().await;
        let raw = std::fs::read(root.join("trace")).map_err(|_| "m7: missing trace")?;
        let mut trace = Trace::default();
        trace.append(&raw)?;
        if let Some(record) = trace.records.iter().find(|e| e["event"] == "muse.progress") {
            return record["task_id"]
                .as_str()
                .map(str::to_owned)
                .ok_or("m7: missing progress ID".into());
        }
    }
}
