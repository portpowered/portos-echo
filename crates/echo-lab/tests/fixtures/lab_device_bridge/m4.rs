//! Controlled bridge boundary for the authored M4 configured CLI journey.
use super::{Error, receive, record, send};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufReader, Write},
    net::TcpStream,
};

pub(super) fn turn(
    wire: &mut BufReader<TcpStream>,
    trace: &mut fs::File,
    screen: &mut TcpStream,
    fixture: &Value,
    contents: &str,
) -> Result<(), Error> {
    if receive(wire)? != "run-pipeline" || receive(wire)? != "audio-chunk" {
        return Err("missing M4 wake/capture".into());
    }
    record(trace, "wyoming.run_started", 1)?;
    record(trace, "run.started", 2)?;
    super::super::spoof::probe(contents, fixture)?;
    for (index, event) in [
        "delegation.created",
        "item/tool/call",
        "tools/call",
        "delegation.context.append",
    ]
    .into_iter()
    .enumerate()
    {
        writeln!(
            trace,
            "{}",
            json!({"event":event,"run_id":"run-1","elapsed_ms":3+index,
            "name":"test.echo","thread_id":"thread-1","turn_id":"turn-1","call_id":"tool-1"})
        )?;
    }
    // Deliberately injected provider latency; not readiness synchronization.
    std::thread::sleep(std::time::Duration::from_millis(
        fixture["reply_delay_ms"].as_u64().unwrap_or(0),
    ));
    writeln!(screen, "data: {}\n", json!({"seq":2,"state":{"state":"speaking"}}))?;
    send(wire.get_mut(), "audio-start", &[])?;
    send(wire.get_mut(), "audio-chunk", &[0, 4, 0, 8])?;
    send(wire.get_mut(), "audio-stop", &[])?;
    if receive(wire)? != "played" {
        return Err("missing M4 Played".into());
    }
    record(trace, "reply.played", 10)?;
    Ok(())
}
