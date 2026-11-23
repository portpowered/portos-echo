//! Synthetic stdin/stdout driver for a prebuilt screen server; no product CLI.
use echo_contracts::secret::Secret;
use echo_screen_contract::UiState;
use echo_screen_server::{ScreenServer, SettingsReply, WifiRelay, serve, serve_with_wifi, settings_channel};
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
    Start {
        token: Secret<String>,
        port: u16,
        #[serde(default)]
        wifi: bool,
    },
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
    let Command::Start { token, port, wifi } = serde_json::from_str(&first)? else {
        return Err("invalid start".into());
    };
    let (tx, rx) = mpsc::channel(32);
    let bind = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut server = if wifi {
        let (settings, mut requests) = settings_channel();
        tokio::spawn(async move {
            while let Some(request) = requests.recv().await {
                request.commit(|_| SettingsReply::Unavailable).await;
            }
        });
        let relay = WifiRelay::new(|body| std::future::ready(wifi_fixture(&body)));
        serve_with_wifi(bind, token, rx, Some(settings), false, Some(relay)).await?
    } else {
        serve(bind, token, rx).await?
    };
    emit(&json!({"ready":{"host":"127.0.0.1", "port":server.bound_addr.port()}}))?;
    drive(&mut server, tx, &mut input).await?;
    server.shutdown().await?;
    emit(&json!({"stopped":true}))
}

fn wifi_fixture(body: &[u8]) -> Option<Vec<u8>> {
    let frame: serde_json::Value = serde_json::from_slice(body).ok()?;
    let mut data = match frame["data"]["op"].as_str()? {
        "status" => json!({"op":"status","ok":true,"ssid":null,"reachable":false}),
        "scan" => json!({"op":"scan","ok":true,"networks":[
            {"ssid":"Fixture B","signalDbm":-42,"secured":true,"networkId":null}
        ]}),
        "join" if frame["data"]["ssid"] == "Fixture B" => {
            json!({"op":"join","ok":false,"error":"wifi-join-failed","ssid":null,"reachable":false})
        }
        _ => return None,
    };
    data["version"] = json!(1);
    Some(json!({"type":"wifi-result","data":data}).to_string().into_bytes())
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
