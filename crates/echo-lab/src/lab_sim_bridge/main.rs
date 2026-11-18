//! Test-purpose Wyoming bridge. No provider, account, or device integration.
mod bridge_trace;

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::process::ExitCode;
use std::time::Duration;

use echo_contracts::wyoming::{AudioFormat, WyomingEvent};
use echo_wyoming::{ClientOptions, EventStream, PlayOutcome, Run, WyomingClient};
use tokio::sync::{mpsc, oneshot};

use bridge_trace::Trace;

fn address(value: &str) -> Result<SocketAddr, String> {
    let addr: SocketAddr = value.parse().map_err(|_| "bridge: invalid Wyoming address")?;
    if addr.ip() != Ipv4Addr::LOCALHOST || addr.port() == 0 {
        return Err("bridge: Wyoming address must be 127.0.0.1 with a nonzero port".into());
    }
    Ok(addr)
}

async fn reply(client: &WyomingClient) -> Result<(), String> {
    let format = AudioFormat {
        rate: 24000,
        width: 2,
        channels: 1,
        extra: std::collections::BTreeMap::new(),
    };
    let (tx, rx) = mpsc::channel(10);
    for _ in 0..10 {
        tx.send(vec![0; 480]).await.map_err(|_| "bridge: reply queue closed")?;
    }
    drop(tx);
    match client.play(format, rx).await.map_err(|_| "bridge: reply failed")? {
        PlayOutcome::Played => Ok(()),
        PlayOutcome::Cut => Err("bridge: unexpected reply cut".into()),
    }
}

async fn capture(client: &WyomingClient, run: &mut Run) -> Result<u64, String> {
    let mut count = 0;
    while count < 10 {
        run.recv()
            .await
            .map_err(|_| "bridge: microphone failed")?
            .ok_or("bridge: microphone ended before ten frames")?;
        count += 1;
    }
    reply(client).await?;
    // Played settles the reply; this one-turn consumer owns the run end.
    client.end_run().await.map_err(|_| "bridge: cannot end run")?;
    while run.recv().await.map_err(|_| "bridge: microphone failed")?.is_some() {
        count += 1;
    }
    if run.stats().dropped != 0 {
        return Err("bridge: microphone overflow; frame count is incomplete".into());
    }
    Ok(count)
}

async fn drain(events: &mut EventStream) -> Result<(), String> {
    loop {
        let event = events
            .recv()
            .await
            .map_err(|_| "bridge: Wyoming stream failed")?
            .ok_or("bridge: Wyoming stream closed before played")?;
        match event.event {
            WyomingEvent::Played(_) => return Ok(()),
            WyomingEvent::RunPipeline(_) => return Err("bridge: overlapping microphone run".into()),
            _ => {}
        }
    }
}

async fn observe(client: &WyomingClient, events: &mut EventStream, trace: &mut Trace) -> Result<(), String> {
    let mut sequence = 0_u64;
    loop {
        let event = events
            .recv()
            .await
            .map_err(|_| "bridge: Wyoming stream failed")?
            .ok_or("bridge: Wyoming stream closed")?;
        if let Some(mut run) = event.run {
            sequence += 1;
            let id = format!("run-{sequence}");
            trace.record("wyoming.run_started", &id, None)?;
            let (count, ()) = tokio::try_join!(capture(client, &mut run), drain(events))?;
            trace.record("wyoming.mic_frames", &id, Some(count))?;
            trace.record("wyoming.run_ended", &id, None)?;
            writeln!(
                io::stdout(),
                "{{\"event\":\"bridge.run_completed\",\"run_id\":\"{id}\"}}"
            )
            .and_then(|()| io::stdout().flush())
            .map_err(|_| "bridge: cannot publish run completion")?;
        }
    }
}

async fn execute() -> Result<(), String> {
    let addr = address(&std::env::var("ECHO_LAB_WYOMING_ADDR").map_err(|_| "bridge: missing Wyoming address")?)?;
    let path = std::env::var_os("ECHO_LAB_TRACE_PATH").ok_or("bridge: missing trace path")?;
    let mut trace = Trace::create(std::path::Path::new(&path))?;
    let mut client = WyomingClient::connect(addr, ClientOptions::default())
        .await
        .map_err(|_| "bridge: cannot connect Wyoming")?;
    let mut events = client.events().map_err(|_| "bridge: cannot take Wyoming events")?;
    client
        .run_satellite()
        .await
        .map_err(|_| "bridge: cannot register Wyoming")?;
    // A describe response after registration is a round-trip readiness barrier.
    client
        .describe()
        .await
        .map_err(|_| "bridge: registration barrier failed")?;
    writeln!(io::stdout(), "{{\"event\":\"bridge.ready\"}}")
        .and_then(|()| io::stdout().flush())
        .map_err(|_| "bridge: cannot publish readiness")?;
    let shutdown = control()?;
    let result = tokio::select! {
        result = observe(&client, &mut events, &mut trace) => result,
        result = shutdown => match result {
            Ok(true) => Ok(()),
            _ => Err("bridge: control channel expects EOF only".into()),
        },
    };
    let cleanup = client
        .close()
        .await
        .map_err(|_| "bridge: Wyoming cleanup failed".to_owned());
    match (result, cleanup) {
        (Err(a), Err(b)) => Err(format!("{a}; {b}")),
        (a, b) => a.and(b),
    }
}

fn control() -> Result<oneshot::Receiver<bool>, String> {
    let (send, receive) = oneshot::channel();
    // Tokio stdin uses an uncancellable blocking task, which would prevent
    // runtime shutdown after a protocol error while the owner retains stdin.
    // This detached OS thread cannot keep the bridge process alive on exit.
    std::thread::Builder::new()
        .name("bridge-control".into())
        .spawn(move || {
            let eof = matches!(io::stdin().read(&mut [0]), Ok(0));
            let _ = send.send(eof);
        })
        .map_err(|_| "bridge: cannot start control reader")?;
    Ok(receive)
}

fn main() -> ExitCode {
    let result = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| "bridge: cannot start runtime".to_owned())
        .and_then(|runtime| {
            runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(120), execute())
                    .await
                    .map_err(|_| "bridge: 120-second safety ceiling elapsed".to_owned())?
            })
        });
    if let Err(error) = result {
        let _ = writeln!(io::stderr(), "echo-lab-sim-bridge: {error}");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_ipv4_loopback_with_a_real_port_is_admitted() {
        assert_eq!(address("127.0.0.1:16700"), Ok(([127, 0, 0, 1], 16700).into()));
        for value in ["localhost:16700", "127.0.0.1:0", "[::1]:16700", "bad"] {
            assert!(address(value).is_err(), "{value}");
        }
    }
}
