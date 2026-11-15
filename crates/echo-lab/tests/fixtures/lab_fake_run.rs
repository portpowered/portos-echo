//! T3 fixture-only tool. Never forwards to a real executable or service.
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};

fn operation(args: &[String]) -> &str {
    match args.first().map(String::as_str) {
        Some("shell") => "firmware",
        Some("-c") => args.get(2).map_or("unknown", String::as_str),
        Some("session") => args.get(1).map_or("unknown", String::as_str),
        Some("message") if args.iter().any(|a| a == "--chirp") => "chirp",
        Some("message") => "play",
        _ => "unknown",
    }
}

fn recording(path: &str) -> io::Result<()> {
    let mut data = Vec::new();
    data.extend(b"RIFF");
    data.extend(44_u32.to_le_bytes());
    data.extend(b"WAVEfmt ");
    data.extend(16_u32.to_le_bytes());
    data.extend(1_u16.to_le_bytes());
    data.extend(1_u16.to_le_bytes());
    data.extend(4_u32.to_le_bytes());
    data.extend(8_u32.to_le_bytes());
    data.extend(2_u16.to_le_bytes());
    data.extend(16_u16.to_le_bytes());
    data.extend(b"data");
    data.extend(8_u32.to_le_bytes());
    for sample in [1_i16, 2, 3, 4] {
        data.extend(sample.to_le_bytes());
    }
    fs::write(path, data)
}

fn response(op: &str, args: &[String], fixture: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    Ok(match op {
        "hold" => json!({"holdId":"fixture-hold", "lease":"fixture-lease", "listening":true, "talking":true}),
        "release" => json!({"holds":[{"id":args.get(2),"state":"ended"}]}),
        "status" => json!({"holds":[{"id":"fixture-hold","state":"live"}]}),
        "wait" => json!({"waited_ms":25000}),
        "clock" => json!({"hour":fixture.get("hour").unwrap_or(&json!(9)),"monotonic_ms":0}),
        "digest" => json!({"sha256":"a".repeat(64)}),
        "reply" => json!({"reply_frames":2}),
        "chirp" | "play" => {
            let index = args.iter().position(|a| a == "--output").ok_or("missing --output")?;
            let output = args.get(index + 1).ok_or("missing output path")?;
            recording(output)?;
            if op == "play" && fixture["block_report"] == true {
                fs::create_dir(
                    std::path::Path::new(output)
                        .parent()
                        .ok_or("no parent")?
                        .join("report.pending"),
                )?;
            }
            json!({"held":true, "playStartedAt":"2026-11-15T09:00:00Z",
                "playEndedAt":"2026-11-15T09:00:00.500Z", "recordStartedAt":"2026-11-15T09:00:00Z",
                "recordEndedAt":"2026-11-15T09:00:01Z"})
        }
        _ => return Err("unsupported fake operation".into()),
    })
}

fn overrides(op: &str, count: usize, fixture: &Value, mut result: Value) -> Value {
    if op == "clock"
        && let Some(hour) = fixture["hours"].get(count)
    {
        result["hour"] = hour.clone();
    }
    if op == "clock"
        && let Some(time) = fixture["times"]
            .get(count)
            .or_else(|| fixture["times"].as_array().and_then(|v| v.last()))
    {
        result["monotonic_ms"] = time.clone();
    }
    if op == "hold"
        && let Some(value) = fixture["holds"].get(count)
    {
        result = value.clone();
    }
    if op == "status" && fixture["preemptions"].as_u64().is_some_and(|n| n > 0) {
        let hold = fixture["holds"]
            .get(count)
            .and_then(|h| h["holdId"].as_str())
            .unwrap_or("fixture-hold");
        result = json!({"holds":[{"id":hold,"state":"ended","ending":{"reason":"preempted"}}]});
    }
    if let Some(value) = fixture.get(op) {
        result = value.clone();
    }
    result
}

fn run() -> Result<u8, Box<dyn std::error::Error>> {
    let fixture: Value = serde_json::from_slice(&fs::read(std::env::var("ECHO_LAB_RUN_FIXTURE")?)?)?;
    let args: Vec<_> = std::env::args().skip(1).collect();
    let op = operation(&args);
    let path = std::env::var("ECHO_LAB_RUN_LEDGER")?;
    let prior = fs::read_to_string(&path).unwrap_or_default();
    let count = prior
        .lines()
        .filter(|line| serde_json::from_str::<Value>(line).is_ok_and(|v| v["operation"] == op))
        .count();
    // The Python source is private implementation, not an invocation argument of interest.
    let ledger_args: Vec<_> = args
        .iter()
        .enumerate()
        .map(|(i, arg)| {
            if args.first().is_some_and(|a| a == "-c") && i == 1 {
                "<host-helper>"
            } else {
                arg.as_str()
            }
        })
        .collect();
    writeln!(
        OpenOptions::new().create(true).append(true).open(path)?,
        "{}",
        json!({"operation":op, "args":ledger_args})
    )?;
    if fixture["cancel_on"] == op {
        fs::write("cancel.signal", b"cancel")?;
        // Explicit fixture notification, then remain blocked until killed.
        loop {
            std::thread::park();
        }
    }
    if op == "play" && u64::try_from(count).unwrap_or(u64::MAX) < fixture["preemptions"].as_u64().unwrap_or(0) {
        return Ok(1);
    }
    if fixture["fail"] == op {
        io::stderr().write_all(b"fixture-private-child-output")?;
        return Ok(1);
    }
    if op == "firmware" {
        writeln!(io::stdout(), "{}", fixture["build"].as_str().unwrap_or("0036811211140"))?;
    } else if fixture["malformed"] == op {
        io::stdout().write_all(b"not JSON fixture-private-child-output")?;
    } else {
        let result = response(op, &args, &fixture)?;
        let result = overrides(op, count, &fixture, result);
        writeln!(io::stdout(), "{result}")?;
    }
    Ok(0)
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(code) => std::process::ExitCode::from(code),
        Err(_) => std::process::ExitCode::FAILURE,
    }
}
