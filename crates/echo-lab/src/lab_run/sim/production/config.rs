//! Private launch files; canonical homes live outside the repository.
use std::{net::SocketAddr, path::Path};
use tokio::process::Command;

pub fn configure(command: &mut Command, root: &Path, address: SocketAddr, live: &str, mcp: &str) -> Result<(), String> {
    configure_at(command, root, root, address, live, mcp)
}

pub fn configure_codex(
    command: &mut Command,
    private: &Path,
    root: &Path,
    address: SocketAddr,
    live: &str,
    mcp: &str,
) -> Result<(), String> {
    configure_at(command, private, root, address, live, mcp)?;
    let path = private.join("configured.private/bridge.toml");
    let text = std::fs::read_to_string(&path).map_err(|_| "m1: cannot read private config")?;
    let text = text.replace("voice = \"alloy\"\n", "");
    let text = format!("{text}\n[codex]\nhome = '{}'\n", private.join("home").display());
    std::fs::write(path, text).map_err(|_| "m1: cannot write private config")?;
    std::fs::write(private.join("configured.private/credential"), "dummy-local-only")
        .map_err(|_| "m1: cannot write dummy key".into())
}

fn configure_at(
    command: &mut Command,
    private: &Path,
    root: &Path,
    address: SocketAddr,
    live: &str,
    mcp: &str,
) -> Result<(), String> {
    let directory = private.join("configured.private");
    std::fs::create_dir(&directory).map_err(|_| "launch: cannot create private configuration")?;
    let template = include_str!("../../../../../../examples/bridge.example.toml")
        // Shared legacy fixtures assert reply-only playback. The M2 configured
        // journey separately exercises the production default-on wake sound.
        .replace("wake_earcon = true", "wake_earcon = false")
        .replace("echo.example.invalid", "127.0.0.1")
        .replace("wyoming_port = 16700", &format!("wyoming_port = {}", address.port()))
        .replace("mode = \"chatgpt\"", "mode = \"api_key\"")
        .replace("voice = \"default\"", "voice = \"alloy\"")
        .replace("0.0.0.0:16900", "127.0.0.1:0") // lint-rules: allow-literal inert example replaced with loopback
        // Each wake gets a fresh pinned call, including after a host restart.
        .replace("session_idle_seconds = 120", "session_idle_seconds = 0")
        // Keep each persistent duplex run's post-reply listen short.
        .replace("session_listen_seconds = 15", "session_listen_seconds = 3")
        .replace("credentials/live.json", "credential")
        .replace("credentials/screen-pairing.txt", "pairing")
        .replace("http://127.0.0.1:18080/mcp", mcp);
    for (name, contents) in [
        ("bridge.toml", template.as_str()),
        ("credential", "synthetic-local-api"),
        ("pairing", "synthetic-local-screen"),
    ] {
        std::fs::write(directory.join(name), contents).map_err(|_| "launch: cannot write private configuration")?;
    }
    for name in [
        "ECHO_LAB_WYOMING_ADDR",
        "ECHO_LAB_LIVE_URL",
        "ECHO_LAB_TRACE_PATH",
        "ECHO_LAB_SESSION_IDLE_SECONDS",
    ] {
        command.env_remove(name);
    }
    let origin = live.trim_end_matches('/');
    command.current_dir(directory).args([
        "--config",
        "bridge.toml",
        "--health-listen",
        "127.0.0.1:0",
        "--live-loopback",
        origin,
        "--model-loopback",
        origin,
    ]);
    command.arg("--trace").arg(root.join("bridge.private.jsonl"));
    Ok(())
}
