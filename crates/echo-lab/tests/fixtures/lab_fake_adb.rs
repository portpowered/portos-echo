//! Synthetic adb boundary. Never invokes adb or contacts a device.
use serde_json::{Value, json};
use std::io::{self, Write};
use std::process::ExitCode;

fn execute() -> Result<u8, Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if let Some(path) = std::env::var_os("ECHO_LAB_FAKE_LEDGER") {
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{}", serde_json::to_string(&args)?)?;
    }
    let overrides: Value = if let Some(path) = std::env::var_os("ECHO_LAB_FAKE_FIXTURE") {
        serde_json::from_slice(&std::fs::read(path)?)?
    } else {
        json!({})
    };
    let defaults: Value = serde_json::from_str(include_str!("../../../../tests/fixtures/fake-adb/adb_responses.json"))?;
    let key = args.join(" ");
    let response = overrides
        .get(&key)
        .or_else(|| defaults.get(&key))
        .ok_or("fake adb: unexpected command")?;
    io::stdout()
        .lock()
        .write_all(response["stdout"].as_str().ok_or("missing stdout")?.as_bytes())?;
    io::stderr()
        .lock()
        .write_all(response["stderr"].as_str().unwrap_or("").as_bytes())?;
    Ok(u8::try_from(response["code"].as_u64().unwrap_or(0))?)
}

fn main() -> ExitCode {
    if let Ok(code) = execute() {
        ExitCode::from(code)
    } else {
        let _ = writeln!(io::stderr().lock(), "synthetic fake adb: invalid fixture or invocation");
        ExitCode::FAILURE
    }
}
