//! T3 fault injection at the nominated bridge process/SSE boundary only.
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{self, BufRead, Read, Write},
    net::{TcpListener, TcpStream},
};

pub fn run(args: &[String], fixture: &Value) -> Result<u8, Box<dyn std::error::Error>> {
    let fault = fixture["device_bridge_fault"].as_str().ok_or("missing fault")?;
    let index = args.iter().position(|s| s == "--trace").ok_or("missing trace")?;
    let path = args.get(index + 1).ok_or("missing trace path")?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let connected = json!({"event":"wyoming.connected","run_id":"connection-1","elapsed_ms":0});
    fs::write(
        path,
        format!(
            "{}\n{}\n",
            connected,
            json!({"event":"screen.listening","elapsed_ms":0,"address":listener.local_addr()?.to_string()})
        ),
    )?;
    wake_trace(path, fault)?;
    writeln!(io::stdout(), "{}", json!({"event":"bridge.ready"}))?;
    if matches!(fault, "late_wake" | "stale_wake") {
        let run = if fault == "stale_wake" { "run-2" } else { "run-1" };
        writeln!(io::stdout(), "{}", json!({"event":"bridge.run_completed","run_id":run}))?;
    }
    io::stdout().flush()?;
    let (mut socket, _) = listener.accept()?;
    authenticated(&socket)?;
    response(&mut socket, fault)?;
    drop(socket);
    // The real owner closes stdin during finalization. Only then inject a late
    // record, proving final oracles use the collected trace, not an early copy.
    io::stdin().read_to_end(&mut Vec::new())?;
    let mut trace = OpenOptions::new().append(true).open(path)?;
    match fault {
        "duplicate" => writeln!(trace, "{connected}")?,
        "incomplete" => trace.write_all(b"{\"event\":")?,
        "error" => writeln!(trace, "{}", json!({"event":"bridge.error","elapsed_ms":1}))?,
        "late_wake" => writeln!(
            trace,
            "{}",
            json!({"event":"wyoming.run_ended","run_id":"run-1","elapsed_ms":3})
        )?,
        _ => (),
    }
    Ok(0)
}

fn wake_trace(path: &str, fault: &str) -> io::Result<()> {
    if matches!(fault, "late_wake" | "stale_wake") {
        let mut trace = OpenOptions::new().append(true).open(path)?;
        for (event, elapsed) in [
            ("wyoming.run_started", 1),
            ("wyoming.mic_frames", 2),
            ("wyoming.run_ended", 3),
        ] {
            writeln!(
                trace,
                "{}",
                json!({"event":event,"run_id":"run-1","elapsed_ms":elapsed,"count":10})
            )?;
        }
    }
    Ok(())
}

fn authenticated(socket: &TcpStream) -> io::Result<()> {
    let mut request = String::new();
    let mut reader = io::BufReader::new(socket);
    reader.read_line(&mut request)?;
    if request != "GET /events?token=synthetic-local-screen HTTP/1.1\r\n" {
        return Err(io::Error::other("unexpected screen authentication"));
    }
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" {
            return Ok(());
        }
    }
}

fn response(socket: &mut TcpStream, fault: &str) -> io::Result<()> {
    if fault == "auth" {
        return socket.write_all(b"HTTP/1.1 401 Unauthorized\r\n\r\n");
    }
    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")?;
    match fault {
        "eof" => Ok(()),
        "malformed" => socket.write_all(b"data: fixture-private-child-output\n\n"),
        "oversized" => socket.write_all(&vec![b'x'; 65_537]),
        _ => writeln!(
            socket,
            "data: {}\n",
            json!({"seq":1,"state":{"state":"ready","message":"Echo Live ready"}})
        ),
    }
}
