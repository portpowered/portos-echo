//! Synthetic trace/stdio owner; playback and bridge coordinate by acknowledgements.
use super::{Value, Write, authenticated, fs, io, json};
use std::{
    io::Read,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

fn root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(PathBuf::from(std::env::var("ECHO_LAB_RUN_FIXTURE")?)
        .parent()
        .ok_or("missing fixture root")?
        .to_path_buf())
}

pub(super) fn run(args: &[String]) -> Result<u8, Box<dyn std::error::Error>> {
    let index = args.iter().position(|s| s == "--trace").ok_or("missing trace")?;
    let path = args.get(index + 1).ok_or("missing trace path")?;
    let root = root()?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    fs::write(
        path,
        format!(
            "{}\n{}\n",
            json!({"event":"wyoming.connected","run_id":"connection-1","elapsed_ms":0}),
            json!({"event":"screen.listening","elapsed_ms":0,"address":listener.local_addr()?.to_string()})
        ),
    )?;
    fs::write(root.join("ring-trace-path"), path)?;
    let fixture: Value = serde_json::from_slice(&fs::read(root.join("fixture.json"))?)?;
    if fixture["ring_prior_wake"] == true {
        writeln!(
            fs::OpenOptions::new().append(true).open(path)?,
            "{}",
            json!({"event":"wyoming.run_started","run_id":"run-previous","elapsed_ms":0})
        )?;
    }
    writeln!(io::stdout(), "{}", json!({"event":"bridge.ready"}))?;
    io::stdout().flush()?;
    let (mut socket, _) = listener.accept()?;
    authenticated(&socket)?;
    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")?;
    writeln!(
        socket,
        "data: {}\n",
        json!({"seq":1,"state":{"state":"ready","message":"Echo Live ready"}})
    )?;
    completions(&root)?;
    Ok(0)
}

fn completions(root: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let closed = Arc::new(AtomicBool::new(false));
    let signal = closed.clone();
    let reader = std::thread::spawn(move || {
        let _ = io::stdin().read_to_end(&mut Vec::new());
        signal.store(true, Ordering::Release);
    });
    while !closed.load(Ordering::Acquire) {
        let pending = root.join("ring-completed");
        if pending.exists() {
            let value: Value = serde_json::from_slice(&fs::read(&pending)?)?;
            fs::remove_file(pending)?;
            writeln!(
                io::stdout(),
                "{}",
                json!({"event":"bridge.run_completed","run_id":value["run"]})
            )?;
            io::stdout().flush()?;
            fs::write(root.join(format!("ring-ack-{}", value["play"])), b"ack")?;
        }
        std::thread::yield_now();
    }
    reader.join().map_err(|_| "fixture reader failed")?;
    Ok(())
}

pub fn play(count: usize, fixture: &Value) -> Result<(), Box<dyn std::error::Error>> {
    let root = root()?;
    if fixture["ring_wakes"].get(count) == Some(&json!(false)) || preempted(fixture, count) {
        return Ok(());
    }
    let path = fs::read_to_string(root.join("ring-trace-path"))?;
    let run = if fixture["stale_ring_run"] == true {
        "run-previous".to_owned()
    } else {
        format!("run-{count}")
    };
    let mut trace = fs::OpenOptions::new().append(true).open(path)?;
    for (offset, event) in ["wyoming.run_started", "wyoming.mic_frames", "wyoming.run_ended"]
        .into_iter()
        .enumerate()
    {
        writeln!(
            trace,
            "{}",
            json!({"event":event,"run_id":run,"elapsed_ms":count * 10 + offset + 1,"count":10})
        )?;
    }
    if fixture["ring_malformed_trace"] == true {
        writeln!(trace, "not JSON")?;
    }
    fs::write(
        root.join("ring-completed.pending"),
        json!({"run":run,"play":count}).to_string(),
    )?;
    fs::rename(root.join("ring-completed.pending"), root.join("ring-completed"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !root.join(format!("ring-ack-{count}")).exists() {
        if std::time::Instant::now() >= deadline {
            return Err("fixture completion not acknowledged".into());
        }
        std::thread::yield_now();
    }
    Ok(())
}

pub fn preempted(fixture: &Value, count: usize) -> bool {
    u64::try_from(count).unwrap_or(u64::MAX) < fixture["preemptions"].as_u64().unwrap_or(0)
        || fixture["preempt_plays"]
            .as_array()
            .is_some_and(|plays| plays.contains(&json!(count)))
}

pub fn host(op: &str, fixture: &Value) -> io::Result<Option<Value>> {
    if op == "release" && fixture["cancel_on_release"] == true {
        fs::write("cancel.signal", b"cancel")?;
    }
    Ok(match op {
        "wake_wait" => Some(json!({"reached":true})),
        "wake_remaining" => Some(json!({"remaining_ms":wake_remaining(fixture)?})),
        _ => None,
    })
}

fn wake_remaining(fixture: &Value) -> io::Result<u64> {
    // Only explicit synthetic misses advance the virtual observation deadline.
    // Native host/wire success cases retain the window and stop on readiness.
    if fixture["device_bridge_fault"] == "ring_replay" {
        return Ok(0);
    }
    let ledger = std::env::var("ECHO_LAB_RUN_LEDGER").map_err(io::Error::other)?;
    let plays = fs::read_to_string(ledger)?
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|row| row["operation"] == "play")
        .count();
    Ok(
        if fixture["ring_wakes"].get(plays.saturating_sub(1)) == Some(&json!(false)) {
            0
        } else {
            6000
        },
    )
}
