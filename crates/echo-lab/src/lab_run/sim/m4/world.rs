//! M4 measurements and shared synthetic configuration; Codex owns runtime peers.
use super::super::trace::Trace;
use echo_spoof_mcp::Spoof;
use serde_json::Value;
use std::path::Path;

#[derive(Default)]
pub struct World {
    pub ui_states: Vec<Value>,
    pub playback: Option<Value>,
    pub reply_pcm_bytes: usize,
    pub trace: Trace,
    pub speaking: Option<Value>,
}

// A finite synthetic 100 ms utterance, distinguishable from startup silence.
// It is sent exactly once through the pinned host's capture backend, not
// inferred from FIFO consumption. The socket observer matches its full sign
// sequence after the host's microphone gain, including cross-frame tails.
pub fn fixture() -> Vec<u8> {
    let mut state = 0x1234_5678_u32;
    (0..1600)
        .flat_map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (if state & 1 == 0 { 1200_i16 } else { -1200_i16 }).to_le_bytes()
        })
        .collect()
}

pub fn configure(root: &Path, port: u16, spoof: &Spoof) -> Result<(), String> {
    let config = format!(
        r#"
# M4/M6 retain their reply-only oracle; enabled acknowledgement is proved by M2.
wake_earcon = false
[echo]
host = "127.0.0.1"
wyoming_port = {port}
[live]
mode = "api_key"
credential_file = "credential"
voice = "alloy"
session_idle_seconds = 120
[agent]
model = "fixture-model"
max_tool_calls = 8
task_deadline_seconds = 10
daily_request_cap = 10
[[mcp.server]]
name = "synthetic"
url = "{}"
auth = "none"
allow_tools = ["test.echo"]
[screen]
listen = "127.0.0.1:0"
pairing_token_file = "pairing"
[audit]
log_dir = "audit"
[chatgpt_auth]
token_endpoint = "http://127.0.0.1:1/token"
"#,
        spoof.url()
    );
    for (name, value) in [
        ("bridge.toml", config.as_str()),
        ("credential", "synthetic-m4-key"),
        ("pairing", "test-pair"),
    ] {
        std::fs::write(root.join(name), value).map_err(|_| "m4: cannot write synthetic configuration")?;
    }
    Ok(())
}

pub fn command(root: &Path, bridge: &Path, live: &str, model: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(bridge);
    command.current_dir(root).args([
        "--config",
        "bridge.toml",
        "--trace",
        "trace",
        "--health-listen",
        "127.0.0.1:0",
        "--live-loopback",
        live,
        "--model-loopback",
        model,
    ]);
    for (key, _) in std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("ECHO_LAB_")) {
        command.env_remove(key);
    }
    command
}
