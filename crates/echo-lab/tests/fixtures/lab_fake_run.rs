//! T3 fixture-only tool. Never forwards to a real executable or service.
#[path = "lab_device_bridge.rs"]
mod device_bridge;
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};

fn operation(args: &[String]) -> &str {
    match args.first().map(String::as_str) {
        Some("shell") => "firmware",
        Some("--config") => "bridge",
        Some("exec-out") => "screencap",
        Some("-c") => args.get(2).map_or("unknown", String::as_str),
        Some("session") => args.get(1).map_or("unknown", String::as_str),
        Some("message") if args.get(1).is_some_and(|v| v == "snapshot") => "snapshot",
        Some("message") if args.get(1).is_some_and(|v| v == "record") => "record",
        Some("message") => "play",
        _ => "stt",
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

fn wait_until(args: &[String]) -> Result<Value, Box<dyn std::error::Error>> {
    let target = args.get(3).ok_or("missing end")?;
    let script = "import datetime,sys; t=datetime.datetime.fromisoformat(sys.argv[1].replace('Z','+00:00')); assert t.tzinfo is not None; print(round(t.timestamp()*1000))"; // lint-rules: allow-literal synthetic timestamp parsing fixture, never a network target
    let output = std::process::Command::new("python")
        .args(["-c", script, target])
        .output()?;
    if !output.status.success() {
        return Err("invalid end".into());
    }
    let end: i64 = std::str::from_utf8(&output.stdout)?.trim().parse()?;
    let now: i64 = fs::read_to_string("wall-ms")
        .unwrap_or_else(|_| "1794733200000".into())
        .parse()?;
    fs::write("wall-ms", now.max(end).to_string())?;
    Ok(json!({"reached":true}))
}

fn response(op: &str, args: &[String], fixture: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    Ok(match op {
        "hold" => json!({"holdId":"fixture-hold", "lease":"fixture-lease", "listening":true, "talking":true}),
        "release" => json!({"holds":[{"id":args.get(2),"state":"ended"}]}),
        "status" => json!({"holds":[{"id":"fixture-hold","state":"live"}]}),
        "wait_until" => wait_until(args)?,
        "wait" => json!({"waited_ms":25000}),
        "clock" => json!({"hour":fixture.get("hour").unwrap_or(&json!(9)),"monotonic_ms":0}),
        "digest" => json!({"sha256":"a".repeat(64)}),
        "reply" => json!({"reply_frames":2}),
        "wav" => json!({"reply_frames":4}),
        "pixels" => json!({"different_pixels":0}),
        "snapshot" => {
            let index = args.iter().position(|a| a == "--output").ok_or("missing --output")?;
            if fixture["missing_image"] != true {
                let bytes = if let Some(path) = fixture["snapshot_file"].as_str() {
                    fs::read(path)?
                } else {
                    include_bytes!("../../../../tests/goldens/lab-frame.jpg").to_vec()
                };
                fs::write(&args[index + 1], bytes)?;
            }
            json!({"success":true,"captured_at":"2026-11-15T09:00:00.600Z"})
        }
        "record" => {
            fs::write("record-ready", b"ready")?;
            let index = args.iter().position(|a| a == "--output").ok_or("missing --output")?;
            recording(&args[index + 1])?;
            if fixture["record_waits_for_frame"] == true {
                // Readiness signal, no elapsed-time synchronization.
                while !std::path::Path::new("frame-ready").exists() {
                    std::thread::yield_now();
                }
            }
            json!({"success":true,"hasAudio":true})
        }
        "play" => {
            let Some(index) = args.iter().position(|a| a == "--output") else {
                return Ok(json!({"held":true,"playEndedAt":"2026-11-15T09:00:00.500Z"}));
            };
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

fn ledger(op: &str, args: &[String]) -> Result<usize, Box<dyn std::error::Error>> {
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
    // One append write keeps concurrent recording/snapshot JSON lines intact.
    let wall: i64 = fs::read_to_string("wall-ms")
        .unwrap_or_else(|_| "1794733200000".into())
        .parse()?;
    let entry = format!("{}\n", json!({"operation":op, "args":ledger_args, "wall_ms":wall}));
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(entry.as_bytes())?;
    Ok(count)
}

// A completed play response with no recording permits a safe status probe.
fn completed_without_recording() -> Result<u8, Box<dyn std::error::Error>> {
    writeln!(
        io::stdout(),
        "{}",
        json!({"held":true,"playEndedAt":"2026-11-15T09:00:00.500Z"})
    )?;
    Ok(0)
}

fn run() -> Result<u8, Box<dyn std::error::Error>> {
    let fixture: Value = serde_json::from_slice(&fs::read(std::env::var("ECHO_LAB_RUN_FIXTURE")?)?)?;
    let args: Vec<_> = std::env::args().skip(1).collect();
    let op = operation(&args);
    let count = ledger(op, &args)?;
    if let Some(code) = configured_fixture(op, &args, &fixture)? {
        return Ok(code);
    }
    if op == "play" {
        fs::write("play-queued", b"synthetic queued playback")?;
    }
    if fixture["oversized"] == op {
        io::stdout().write_all(&vec![b'x'; 1024 * 1024 + 1])?;
        return Ok(0);
    }
    if fixture["cancel_on"] == op && (fixture["cancel_once"] != true || count == 0) {
        fs::write("cancel.signal", b"cancel")?;
        // Explicit fixture notification, then remain blocked until killed.
        loop {
            std::thread::park();
        }
    }
    if op == "play" && u64::try_from(count).unwrap_or(u64::MAX) < fixture["preemptions"].as_u64().unwrap_or(0) {
        return completed_without_recording();
    }
    if fixture["fail"] == op {
        io::stderr().write_all(b"fixture-private-child-output")?;
        return Ok(1);
    }
    if op == "snapshot" {
        fs::write("frame-ready", b"ready")?;
    }
    if fixture["real_pixels"] == true && op == "pixels" {
        let output = std::process::Command::new("python").args(&args).output()?;
        io::stdout().write_all(&output.stdout)?;
        return Ok(u8::from(!output.status.success()));
    } else if op == "stt" {
        io::stdout().write_all(
            fixture["transcript"]
                .as_str()
                .unwrap_or("The time is 7:49 p.m.") // lint-rules: allow-literal synthetic STT stdout, never a network target
                .as_bytes(),
        )?;
    } else if op == "screencap" {
        let file = fixture["screencap_file"].as_str().ok_or("missing screencap fixture")?;
        io::stdout().write_all(&fs::read(file)?)?;
    } else if op == "firmware" {
        let value = match args.get(2).map(String::as_str) {
            Some("ro.bootmode") => fixture["bootmode"].as_str().unwrap_or("normal"),
            Some("sys.boot_completed") => fixture["boot_completed"].as_str().unwrap_or("1"),
            _ => fixture["build"].as_str().unwrap_or("0036811211140"),
        };
        writeln!(io::stdout(), "{value}")?;
    } else if fixture["malformed"] == op {
        io::stdout().write_all(b"not JSON fixture-private-child-output")?;
    } else {
        let result = response(op, &args, &fixture)?;
        let result = overrides(op, count, &fixture, result);
        writeln!(io::stdout(), "{result}")?;
    }
    Ok(0)
}

fn configured_fixture(op: &str, args: &[String], fixture: &Value) -> Result<Option<u8>, Box<dyn std::error::Error>> {
    if args.first().is_some_and(|arg| arg == "forward") && fixture["wire_port"].is_u64() {
        let dynamic_port = "tcp:0"; // lint-rules: allow-literal inert fake adb argument comparison, never a connection target
        if args.get(1).is_some_and(|arg| arg == dynamic_port) {
            writeln!(io::stdout(), "{}", fixture["wire_port"])?;
        }
        return Ok(Some(0));
    }
    if op == "firmware" && matches!(args.get(1).map(String::as_str), Some("ps" | "kill")) {
        return device_bridge::adb(args, fixture).map(Some);
    }
    if op == "bridge" && fixture["device_bridge_fault"].is_string() {
        return device_bridge::run(args, fixture).map(Some);
    }
    if op == "play"
        && let Ok(address) = std::env::var("ECHO_LAB_DEVICE_CONTROL")
    {
        device_wake(&address, args)?;
    }
    if real_config(op, args, fixture) {
        let output = std::process::Command::new("python").args(args).output()?;
        io::stdout().write_all(&output.stdout)?;
        return Ok(Some(u8::from(!output.status.success())));
    }
    Ok(None)
}

fn real_config(op: &str, args: &[String], fixture: &Value) -> bool {
    args.first().is_some_and(|arg| arg == "-c")
        && fixture["real_config"] == true
        && !matches!(
            op,
            "clock" | "digest" | "reply" | "wav" | "wait_until" | "wait" | "pixels"
        )
}

fn device_wake(address: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{BufRead, BufReader};
    let address: std::net::SocketAddr = address.parse()?;
    if address.ip() != std::net::Ipv4Addr::LOCALHOST {
        return Err("nonlocal fake control".into());
    }
    let index = args.iter().position(|a| a == "--file").ok_or("missing file")?;
    let bytes = fs::read(&args[index + 1])?;
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" {
        return Err("expected WAV".into());
    }
    let mut stream = std::net::TcpStream::connect(address)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(20)))?;
    writeln!(stream, "{}", json!({"pcm":bytes[44..]}))?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    if reply.trim() != "ok" {
        return Err("fake host failed".into());
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(code) => std::process::ExitCode::from(code),
        Err(_) => std::process::ExitCode::FAILURE,
    }
}
