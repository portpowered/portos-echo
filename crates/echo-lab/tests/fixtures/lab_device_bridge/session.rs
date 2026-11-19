//! Synthetic bridge boundary: actual socket traffic, one run and an idle timer.
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
};
type Error = Box<dyn std::error::Error>;

pub(super) fn run(args: &[String], fixture: &Value, contents: &str) -> Result<u8, Error> {
    let config = echo_config::BridgeConfig::from_toml_str(contents)?;
    let address = (config.echo.host.as_str(), config.echo.wyoming_port);
    if address.0 != "127.0.0.1" {
        return Err("nonlocal synthetic wire".into());
    }
    let mut wire = BufReader::new(TcpStream::connect(address)?);
    wire.get_mut()
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let index = args.iter().position(|s| s == "--trace").ok_or("missing trace")?;
    let path = args.get(index + 1).ok_or("missing trace path")?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    let mut trace = fs::File::create(path)?;
    writeln!(
        trace,
        "{}",
        json!({"event":"wyoming.connected","run_id":"connection-1","elapsed_ms":0})
    )?;
    writeln!(
        trace,
        "{}",
        json!({"event":"screen.listening","elapsed_ms":0,"address":listener.local_addr()?.to_string()})
    )?;
    writeln!(io::stdout(), "{}", json!({"event":"bridge.ready"}))?;
    io::stdout().flush()?;
    let (mut screen, _) = listener.accept()?;
    super::authenticated(&screen)?;
    screen.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")?;
    writeln!(
        screen,
        "data: {}\n",
        json!({"seq":1,"state":{"state":"ready","message":"Echo Live ready"}})
    )?;
    turns(&mut wire, &mut trace, fixture)?;
    finish(
        &mut wire,
        &mut trace,
        fixture,
        u64::from(config.voice.session_listen_seconds),
    )?;
    Ok(0)
}

fn finish(wire: &mut BufReader<TcpStream>, trace: &mut fs::File, fixture: &Value, listen: u64) -> Result<(), Error> {
    // This is the configured idle behavior, not fixture scheduling readiness.
    let idle = fixture["idle_ms"].as_u64().unwrap_or(listen * 1000);
    std::thread::sleep(std::time::Duration::from_millis(idle));
    if fixture["missing_end"] != true {
        send(wire.get_mut(), "hassmic-end", &[])?;
    }
    record(trace, "wyoming.mic_frames", 90)?;
    record(trace, "wyoming.run_ended", 100)?;
    writeln!(
        io::stdout(),
        "{}",
        json!({"event":"bridge.run_completed","run_id":"run-1"})
    )?;
    io::stdout().flush()?;
    io::stdin().read_to_end(&mut Vec::new())?;
    if fixture["late_error"] == true {
        record(trace, "bridge.error", 101)?;
    }
    Ok(())
}

fn turns(wire: &mut BufReader<TcpStream>, trace: &mut fs::File, fixture: &Value) -> Result<(), Error> {
    if receive(wire)? != "run-pipeline" {
        return Err("missing wake".into());
    }
    record(trace, "wyoming.run_started", 1)?;
    record(trace, "run.started", 2)?;
    for turn in 0..fixture["session_replies"].as_u64().ok_or("missing reply count")? {
        if receive(wire)? != "audio-chunk" {
            return Err("missing turn capture".into());
        }
        send(wire.get_mut(), "audio-start", &[])?;
        send(wire.get_mut(), "audio-chunk", &[0, 4, 0, 8])?;
        send(wire.get_mut(), "audio-stop", &[])?;
        if receive(wire)? != "played" {
            return Err("missing independent Played".into());
        }
        record(trace, "reply.played", 10 + turn)?;
    }
    Ok(())
}

fn record(trace: &mut fs::File, event: &str, elapsed: u64) -> io::Result<()> {
    writeln!(
        trace,
        "{}",
        json!({"event":event,"run_id":"run-1","elapsed_ms":elapsed,"count":10,"outcome":"played"})
    )
}

pub(super) fn send(socket: &mut TcpStream, kind: &str, payload: &[u8]) -> io::Result<()> {
    use echo_contracts::wyoming::{Event, Header, encode};
    socket.write_all(&encode(&Event {
        header: Header {
            r#type: kind.into(),
            data: Some(json!({"rate":16000,"width":2,"channels":1})),
            data_length: 0,
            payload_length: payload.len(),
        },
        data: None,
        payload: payload.to_vec(),
    }))
}

pub(super) fn receive(socket: &mut BufReader<TcpStream>) -> Result<String, Error> {
    let mut header = String::new();
    socket.read_line(&mut header)?;
    let header: echo_contracts::wyoming::Header = serde_json::from_str(&header)?;
    let mut payload = vec![0; header.data_length + header.payload_length];
    socket.read_exact(&mut payload)?;
    Ok(header.r#type)
}
