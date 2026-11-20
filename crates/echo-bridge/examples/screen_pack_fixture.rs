//! T3 stdin driver: production settings owner and pack lifetime, no voice/provider.
use echo_bridge::bridge_settings::{SettingsHost, SettingsService};
use echo_config::{BridgeConfig, HomeConfig, SettingsOverlay};
use echo_contracts::secret::Secret;
use echo_screen_contract::{
    UiState,
    settings_v1::{LinkStatusV1, PromptTextsV1},
};
use echo_screen_server::{ScreenServer, serve_with_settings};
use serde_json::{Value, json};
use std::{error::Error, io::Write, path::PathBuf};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::mpsc,
};

type Result<T = ()> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
type Input = tokio::io::Lines<BufReader<tokio::io::Stdin>>;
const BASE: &str = "[echo]\nhost='127.0.0.1'\n[live]\nvoice='alloy'\ncredential_file='absent/live'\n[agent]\n[mcp]\n[screen]\npairing_token_file='absent/pairing'\n[audit]\nlog_dir='absent/logs'\n";

fn emit(value: &Value) -> Result {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn path(value: &Value, key: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(value[key].as_str().ok_or("missing path")?))
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    if run().await.is_err() {
        let _ = std::io::stderr().write_all(b"pack fixture failed\n");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

async fn run() -> Result {
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    let start: Value = serde_json::from_str(&input.next_line().await?.ok_or("missing start")?)?;
    let home = path(&start, "home")?;
    let ca = path(&start, "ca")?;
    let (owner, mut server, states) = open(home.clone()).await?;
    let _jobs = server.install_configured_packs(home, Some(ca), vec![]);
    emit(&json!({"ready":server.bound_addr.port()}))?;
    let (result, completion) = owner
        .scope(
            async {
                let result = drive(&mut server, states, &mut input).await;
                server.shutdown().await?;
                result
            },
            || LinkStatusV1::Unconfigured,
        )
        .await;
    result?;
    emit(&json!({"stopped":true,"restart_requested":completion.restart_requested}))
}

async fn open(home: PathBuf) -> Result<(SettingsService, ScreenServer, mpsc::Sender<UiState>)> {
    let mut base = BridgeConfig::from_toml_str(BASE)?;
    base.home = Some(HomeConfig {
        dir: home,
        agent_tools: Default::default(),
    });
    let host = SettingsHost::open(
        base,
        SettingsOverlay::default(),
        PromptTextsV1 {
            voice: echo_config::LIVE_INSTRUCTIONS.into(),
            runner: echo_config::RUNNER_INSTRUCTIONS.into(),
        },
        vec![],
    )?;
    let (owner, send) = SettingsService::resume(Box::new(host));
    let (states, receiver) = mpsc::channel(32);
    let server = serve_with_settings(
        "127.0.0.1:0".parse()?,
        Secret::new("synthetic-pack-pair".into()),
        receiver,
        Some(send),
    )
    .await?;
    Ok((owner, server, states))
}

async fn drive(server: &mut ScreenServer, states: mpsc::Sender<UiState>, input: &mut Input) -> Result {
    loop {
        tokio::select! {
            line = input.next_line() => {
                let Some(line) = line? else { return Err("unexpected eof".into()); };
                let command: Value = serde_json::from_str(&line)?;
                if command["stop"] == true { return Ok(()); }
                states.send(serde_json::from_value(command["state"].clone())?).await?;
                emit(&json!({"sent":true}))?;
            }
            action = server.actions.recv() => {
                emit(&json!({"action":action.ok_or("action channel closed")?}))?;
            }
        }
    }
}
