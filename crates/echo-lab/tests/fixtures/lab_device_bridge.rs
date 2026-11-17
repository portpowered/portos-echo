//! T3 fault injection at the nominated bridge process/SSE boundary only.
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{self, BufRead, Read, Write},
    net::{TcpListener, TcpStream},
};

pub fn adb(args: &[String], fixture: &Value) -> Result<u8, Box<dyn std::error::Error>> {
    if args.get(1).is_some_and(|v| v == "ps")
        && let Some(rows) = fixture["device_ps"].as_str()
    {
        writeln!(io::stdout(), "{rows}")?;
        return Ok(0);
    }
    if args.get(1).is_some_and(|v| v == "kill") {
        if fixture["device_cancel_kill"] == true {
            fs::write("cancel.signal", "")?;
            return Ok(0);
        }
        if fixture["device_fail_kill"] == true {
            return Ok(1);
        }
        if fixture["device_no_reconnect"] == true {
            return Ok(0);
        }
    }
    let address: std::net::SocketAddr = std::env::var("ECHO_LAB_DEVICE_CONTROL")?.parse()?;
    if address.ip() != std::net::Ipv4Addr::LOCALHOST {
        return Err("nonlocal fake control".into());
    }
    let command = if args == ["shell", "ps", "-A", "-o", "PID,PPID,NAME"] {
        json!({"action":"identity"})
    } else if args.len() == 3 && args[1] == "kill" {
        json!({"action":"kill","pid":args[2].parse::<u32>()?})
    } else {
        return Err("unexpected adb command".into());
    };
    let mut socket = TcpStream::connect(address)?;
    socket.set_read_timeout(Some(std::time::Duration::from_secs(20)))?;
    writeln!(socket, "{command}")?;
    let mut line = String::new();
    io::BufReader::new(socket).read_line(&mut line)?;
    let response: Value = serde_json::from_str(&line)?;
    if response["event"] != "host.ack" {
        return Err("host control failed".into());
    }
    if args[1] == "ps" {
        let pid = response["pid"].as_u64().ok_or("missing pid")?;
        writeln!(io::stdout(), "PID PPID NAME\n2 1 hassmic\n{pid} 2 hassmic")?;
    }
    Ok(0)
}

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
    writeln!(io::stdout(), "{}", json!({"event":"bridge.ready"}))?;
    io::stdout().flush()?;
    let (mut socket, _) = listener.accept()?;
    authenticated(&socket)?;
    response(&mut socket, fault)?;
    drop(socket);
    if matches!(fault, "late_wake" | "stale_wake") {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !std::path::Path::new("../../play-queued").exists() {
            if std::time::Instant::now() >= deadline {
                return Err("play never queued".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        wake_trace(path, fault)?;
        let run = if fault == "stale_wake" { "run-2" } else { "run-1" };
        writeln!(io::stdout(), "{}", json!({"event":"bridge.run_completed","run_id":run}))?;
        io::stdout().flush()?;
    }
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
        "missing_window" | "missing_muted" => {
            let states = if fault == "missing_window" {
                vec![
                    json!({"state":"ready","message":"Echo Live ready"}),
                    json!({"state":"muted"}),
                    json!({"state":"ready","message":"Echo Live ready"}),
                ]
            } else {
                vec![json!({"state":"ready","message":"Echo Live ready"})]
            };
            for (index, state) in states.into_iter().enumerate() {
                writeln!(socket, "data: {}\n", json!({"seq":index+1,"state":state}))?;
            }
            Ok(())
        }
        _ => writeln!(
            socket,
            "data: {}\n",
            json!({"seq":1,"state":{"state":"ready","message":"Echo Live ready"}})
        ),
    }
}
