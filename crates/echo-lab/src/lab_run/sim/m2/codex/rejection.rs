//! T3 canonical auth rejection: no device is admitted before authentication.
use super::{config, provider, read_trace};
use crate::lab_run::{
    run_options::Options,
    sim::{controlled, production::Screen},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use echo_codex::codex_supervisor::{Supervisor, SupervisorConfig};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{
    net::TcpListener,
    process::{Child, Command},
};

#[derive(Default)]
struct Case {
    child: Option<Child>,
    provider: Option<provider::Provider>,
    root: Option<config::Root>,
    listener: Option<TcpListener>,
    screen: Option<Screen>,
    measured: Value,
    binary: PathBuf,
    auth: Vec<u8>,
}

impl Case {
    async fn start(&mut self, options: &Options, case: &str) -> Result<(), String> {
        self.root = Some(config::Root::new()?);
        let root = &self.root.as_ref().ok_or("m2: missing root")?.0;
        self.listener = Some(
            TcpListener::bind("127.0.0.1:0")
                .await
                .map_err(|_| "m2: listener bind")?,
        );
        let port = self
            .listener
            .as_ref()
            .ok_or("m2: missing listener")?
            .local_addr()
            .map_err(|_| "m2: listener address")?
            .port();
        self.provider = Some(provider::Provider::start(&options.python).await?);
        let origin = &self.provider.as_ref().ok_or("m2: missing provider")?.origin;
        let mut command = Command::new(options.bridge.as_deref().ok_or("m2: missing bridge")?);
        config::configure(&mut command, root, port, origin, &root.join("trace"))?;
        self.binary = binary().await?;
        provision(root, &self.binary, case, &mut self.auth)?;
        self.child = Some(
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(|_| "m2: bridge launch")?,
        );
        self.measured = json!({"case":case,"runtime":"codex","calls":null,"refreshes":null,"frames":[],"trace":[]});
        Ok(())
    }

    async fn observe(&mut self) -> Result<(), String> {
        let root = &self.root.as_ref().ok_or("m2: missing root")?.0;
        loop {
            if let Ok(trace) = read_trace(&root.join("trace"))
                && trace.records.iter().any(|r| r["event"] == "auth.relink_required")
            {
                let address = trace
                    .records
                    .iter()
                    .find(|r| r["event"] == "screen.listening")
                    .and_then(|r| r["address"].as_str())
                    .ok_or("m2: missing screen address")?
                    .parse()
                    .map_err(|_| "m2: invalid screen address")?;
                self.screen = Some(Screen::connect(address, false).await?);
                self.measured["frames"] = json!([self.screen.as_ref().ok_or("m2: missing screen")?.current()?]);
                return Ok(());
            }
            if self
                .child
                .as_mut()
                .ok_or("m2: missing child")?
                .try_wait()
                .map_err(|_| "m2: child wait")?
                .is_some()
            {
                return Err("m2: bridge exited before relink observation".into());
            }
            tokio::task::yield_now().await;
        }
    }

    async fn close_child(&mut self) -> Result<(), String> {
        if let Some(mut child) = self.child.take() {
            drop(child.stdin.take());
            if tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
                .await
                .is_err()
            {
                child.kill().await.map_err(|_| "m2: child forced collection failed")?;
                return Err("m2: authorization shutdown exceeded cleanup deadline".into());
            }
            let result = child.wait_with_output().await.map_err(|_| "m2: child collection")?;
            self.measured["exit_code"] = json!(result.status.code());
            // Publish only validated bounded diagnostics, never arbitrary child output.
            if result.stdout != b"" || result.stderr != b"echo-bridge: authorization\n" {
                return Err("m2: unexpected authorization exit output".into());
            }
            self.measured["stdout"] = json!("");
            self.measured["stderr"] = json!("echo-bridge: authorization\n");
        }
        if let Some(screen) = &mut self.screen
            && screen.next().await != Err("screen: stream ended".into())
        {
            return Err("m2: unexpected post-relink screen frame".into());
        }
        Ok(())
    }

    async fn close(&mut self) -> Result<(), String> {
        let child = self.close_child().await;
        let provider = self.close_provider().await;
        let observed = self.final_observations().await;
        child.and(provider).and(observed)
    }

    async fn close_provider(&mut self) -> Result<(), String> {
        if let Some(provider) = self.provider.take() {
            let observed = provider.close().await?;
            self.measured["calls"] = json!(observed["calls"].as_array().map(Vec::len));
            self.measured["provider_wire"] = json!(observed["calls"] == json!([]));
            for name in ["sidebands", "external"] {
                self.measured[name] = observed[name].clone();
            }
            self.measured["refreshes"] = observed["refreshes"].clone();
        }
        Ok(())
    }

    async fn final_observations(&mut self) -> Result<(), String> {
        if let Some(listener) = self.listener.take() {
            self.measured["wyoming_admissions"] = json!(
                std::future::poll_fn(|cx| std::task::Poll::Ready(u64::from(listener.poll_accept(cx).is_ready()))).await
            );
        }
        if let Some(root) = &self.root {
            self.measured["trace"] = json!(read_trace(&root.0.join("trace"))?.records);
            self.measured["auth_unchanged"] =
                json!(std::fs::read(root.0.join("home/auth.json")).map_err(|_| "m2: missing auth")? == self.auth);
            let owner = Supervisor::new(SupervisorConfig::new(self.binary.clone(), root.0.join("home")))
                .map_err(|_| "m2: ownership not released")?;
            drop(owner);
            self.measured["home_released"] = json!(true);
        }
        Ok(())
    }
}

pub(in crate::lab_run::sim::m2) async fn run(options: &Options, case: &str) -> (Value, Result<(), String>) {
    let mut state = Case::default();
    let work = controlled(
        async {
            state.start(options, case).await?;
            state.observe().await
        },
        options.cancel.as_deref(),
        tokio::time::Instant::now() + super::super::DEADLINE,
    )
    .await;
    let closed = state.close().await;
    let result = work.and(closed).and_then(|()| super::oracle::check(&state.measured));
    (state.measured.clone(), result)
}

fn provision(root: &Path, binary: &Path, case: &str, auth: &mut Vec<u8>) -> Result<(), String> {
    let path = root.join("bridge.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|_| "m2: missing config")?
        .replace("mode = \"api_key\"", "mode = \"chatgpt\"");
    std::fs::write(path, text).map_err(|_| "m2: cannot configure ChatGPT")?;
    // Screen's public observer uses this inert pairing token.
    std::fs::write(root.join("pairing"), "synthetic-local-screen").map_err(|_| "m2: pairing fixture")?;
    drop(Supervisor::new(SupervisorConfig::new(binary.into(), root.join("home"))).map_err(|_| "m2: home provision")?);
    *auth = if case == "invalid" {
        b"malformed synthetic store".to_vec()
    } else {
        synthetic()?
    };
    std::fs::write(root.join("home/auth.json"), auth).map_err(|_| "m2: synthetic store write".into())
}

fn synthetic() -> Result<Vec<u8>, String> {
    let claims = json!({"exp":1,"email":"local@example.invalid",
        "https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-local","chatgpt_plan_type":"plus"}}); // lint-rules: allow-literal inert synthetic JWT claim key
    let token = format!("e30.{}.synthetic", URL_SAFE_NO_PAD.encode(claims.to_string()));
    serde_json::to_vec(&json!({"auth_mode":"chatgpt","OPENAI_API_KEY":null,
        "tokens":{"id_token":token,"access_token":token,"refresh_token":"synthetic-refresh",
            "account_id":"synthetic-local"},"last_refresh":"2000-01-01T00:00:00Z"}))
    .map_err(|_| "m2: synthetic store encoding".into())
}

async fn binary() -> Result<PathBuf, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut command = {
        let mut command = Command::new("sh");
        command.arg(root.join("scripts/codex-app-server.sh"));
        command
    };
    let output = command
        .env("ECHO_CODEX_OFFLINE", "1")
        .output()
        .await
        .map_err(|_| "m2: pin resolver")?;
    if !output.status.success() {
        return Err("m2: offline verified pin unavailable".into());
    }
    Ok(String::from_utf8(output.stdout)
        .map_err(|_| "m2: invalid pin path")?
        .trim()
        .into())
}
