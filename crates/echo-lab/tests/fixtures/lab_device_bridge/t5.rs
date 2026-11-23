//! Synthetic M2 bridge: real process, loopback SSE, no provider or device call.
use super::{TcpListener, Value, Write, authenticated, fs, io, json};
use std::io::Read;
#[path = "session.rs"]
mod session;
#[path = "spoof.rs"]
mod spoof;

pub(super) fn run(args: &[String], fixture: &Value) -> Result<u8, Box<dyn std::error::Error>> {
    let contents = launch_record(args, fixture)?;
    if fixture["session_replies"].is_u64() {
        return session::run(args, fixture, &contents);
    }
    let index = args.iter().position(|s| s == "--trace").ok_or("missing trace")?;
    let path = args.get(index + 1).ok_or("missing trace path")?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let mut trace = vec![
        json!({"event":"wyoming.connected","run_id":"connection-1","elapsed_ms":0}),
        json!({"event":"screen.listening","elapsed_ms":0,"address":listener.local_addr()?.to_string()}),
    ];
    for (event, elapsed) in [
        ("wyoming.run_started", 1),
        ("wyoming.mic_frames", 2),
        ("wyoming.run_ended", 3),
    ] {
        trace.push(json!({"event":event,"run_id":"run-1","elapsed_ms":elapsed,"count":10}));
    }
    trace.push(json!({"event":"live.closed","elapsed_ms":4,"reason":"idle"}));
    let bytes = trace.iter().map(|row| format!("{row}\n")).collect::<Vec<_>>().concat();
    fs::write(path, format!("{}\n{}\n", trace[0], trace[1]))?;
    writeln!(io::stdout(), "{}", json!({"event":"bridge.ready"}))?;
    io::stdout().flush()?;
    let (mut socket, _) = listener.accept()?;
    authenticated(&socket)?;
    present(&mut socket, fixture)?;
    if fixture["screen_fault"] != "auth" {
        if fixture["spoof_probe"] == true {
            spoof::probe(&contents, fixture)?;
        }
        fs::write(path, bytes)?;
        writeln!(
            io::stdout(),
            "{}",
            json!({"event":"bridge.run_completed","run_id":"run-1"})
        )?;
        io::stdout().flush()?;
    }
    io::stdin().read_to_end(&mut Vec::new())?;
    if fixture["late_error"] == true {
        let mut file = fs::OpenOptions::new().append(true).open(path)?;
        writeln!(file, "{}", json!({"event":"bridge.error","elapsed_ms":5}))?;
    }
    Ok(0)
}

fn launch_record(args: &[String], fixture: &Value) -> Result<String, Box<dyn std::error::Error>> {
    let index = args.iter().position(|s| s == "--config").ok_or("missing config")?;
    let config_path = args.get(index + 1).ok_or("missing config path")?;
    let contents = fs::read_to_string(config_path)?;
    fs::write(
        fixture["launch_record"].as_str().ok_or("missing launch record")?,
        json!({"args":args,"cwd":std::env::current_dir()?,"config":contents}).to_string(),
    )?;
    Ok(contents)
}

fn present(socket: &mut std::net::TcpStream, fixture: &Value) -> io::Result<()> {
    if fixture["screen_fault"] == "auth" {
        return socket.write_all(b"HTTP/1.1 401 Unauthorized\r\n\r\n");
    }
    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")?;
    let states = [
        json!({"state":"ready","message":"Echo Live ready"}),
        json!({"state":"listening"}),
        json!({"state":"connected"}),
        json!({"state":"speaking"}),
        json!({"state":"connected"}),
        json!({"state":"ready","message":"Echo Live ready"}),
        json!({"state":"ready","message":"Echo Live ready"}),
    ];
    for (index, mut state) in states.into_iter().enumerate() {
        if index == 1 {
            wait_for_play(fixture)?;
        }
        if index == 2 && fixture["screen_fault"] == "missing" {
            continue;
        }
        if index == 2 && fixture["screen_fault"] == "wrong" {
            state = json!({"state":"listening"});
        }
        let seq = if index == 2 && fixture["screen_fault"] == "stale" {
            2
        } else {
            index + 1
        };
        writeln!(socket, "data: {}\n", json!({"seq":seq,"state":state}))?;
    }
    Ok(())
}

#[allow(clippy::disallowed_methods, reason = "Host time for test")]
fn wait_for_play(fixture: &Value) -> io::Result<()> {
    let root = std::path::Path::new(
        fixture["launch_record"]
            .as_str()
            .ok_or_else(|| io::Error::other("missing record"))?,
    )
    .parent()
    .ok_or_else(|| io::Error::other("missing root"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !root.join("play-queued").exists() {
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::other("play never queued"));
        }
        std::thread::yield_now();
    }
    Ok(())
}
