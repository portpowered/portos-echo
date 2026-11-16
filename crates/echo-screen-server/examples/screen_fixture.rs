//! Synthetic stdin/stdout driver for a prebuilt screen server; no product CLI.
use echo_contracts::secret::Secret;
use echo_screen_contract::UiState;
use echo_screen_server::{ScreenServer, serve};
use serde::Deserialize;
use serde_json::json;
use std::{
    error::Error,
    io::Write,
    net::{Ipv4Addr, SocketAddr},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::mpsc,
};

type Result<T = ()> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
enum Command {
    Start { token: Secret<String>, port: u16 },
    State(UiState),
    Revoke(bool),
    Stop(bool),
}

fn emit(value: &serde_json::Value) -> Result {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer(&mut out, value)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    if run().await.is_err() {
        let _ = std::io::stderr().write_all(b"screen fixture failed\n");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

async fn run() -> Result {
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    let first = input.next_line().await?.ok_or("missing start")?;
    let Command::Start { token, port } = serde_json::from_str(&first)? else {
        return Err("invalid start".into());
    };
    let (tx, rx) = mpsc::channel(32);
    let mut server = serve(SocketAddr::from((Ipv4Addr::LOCALHOST, port)), token, rx).await?;
    emit(&json!({"ready":{"host":"127.0.0.1", "port":server.bound_addr.port()}}))?;
    drive(&mut server, tx, &mut input).await?;
    server.shutdown().await?;
    emit(&json!({"stopped":true}))
}

async fn drive(
    server: &mut ScreenServer,
    tx: mpsc::Sender<UiState>,
    input: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
) -> Result {
    loop {
        tokio::select! {
            line = input.next_line() => {
                let Some(line) = line? else { return Ok(()); };
                match serde_json::from_str::<Command>(&line)? {
                    Command::State(state) => tx.send(state).await?,
                    Command::Revoke(true) => {
                        server.revoke_pairing().await?;
                        emit(&json!({"revoked":true}))?;
                    }
                    Command::Stop(true) => return Ok(()),
                    _ => return Err("invalid command".into()),
                }
            }
            action = server.actions.recv() => {
                let Some(action) = action else { return Err("action channel closed".into()); };
                emit(&json!({"action":action}))?;
            }
        }
    }
}
