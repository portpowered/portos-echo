//! One Live call owner drives WebRTC while Wyoming owns its bounded wire queues.
mod bridge_pacing;
mod journey;

use crate::{
    Error,
    bridge_health::Health,
    bridge_input::{Provider, Settings},
    bridge_trace::Trace,
};
use echo_contracts::{ids::SessionId, wyoming::WyomingEvent};
use echo_live::{
    LivePeer,
    call::{LiveAuthMode, LiveCallFactory, SessionOptions},
};
use echo_wyoming::{ClientOptions, EventStream, WyomingClient};
use serde_json::json;
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

/// # Errors
/// All failures use fixed categories. EOF closes owned tasks and sockets.
pub async fn execute(settings: Settings) -> Result<(), Error> {
    let mut trace = Trace::open(
        &settings.trace,
        match settings.mode {
            LiveAuthMode::Chatgpt => "chatgpt",
            LiveAuthMode::ApiKey => "api_key",
        },
    )?;
    let health = Health::bind(settings.health).await?;
    trace.record(
        "bridge.started",
        None,
        json!({"health_addr":health.address.to_string()}),
    )?;
    let mut shutdown = control()?;
    let work = async {
        let mut client = tokio::select! {
            result = WyomingClient::connect(settings.wyoming, ClientOptions::default()) => result?,
            eof = &mut shutdown => return match eof { Ok(true) => Ok(()), _ => Err(Error::Control) },
        };
        let result = tokio::select! {
            result = connected(&mut client, settings, &health, &mut trace) => result,
            eof = &mut shutdown => match eof { Ok(true) => Ok(()), _ => Err(Error::Control) },
        };
        let cleanup = client.close().await.map_err(Error::from);
        result.and(cleanup)
    };
    let result = work.await;
    let report = result.as_ref().err().map_or(Ok(()), |error| {
        trace.record("bridge.error", None, json!({"reason":error.to_string()}))
    });
    health.close().await;
    result.and(report)
}

async fn connected(
    client: &mut WyomingClient,
    settings: Settings,
    health: &Health,
    trace: &mut Trace,
) -> Result<(), Error> {
    let mut events = client.events()?;
    client.run_satellite().await?;
    client.describe().await?;
    health.state.send_modify(|state| state.wyoming = "connected");
    trace.record("wyoming.connected", Some("connection-1"), json!({}))?;
    publish("bridge.ready", None)?;
    let factory = LiveCallFactory::new(&settings.live_url, settings.mode, settings.credential)?;
    observe(
        client,
        &mut events,
        &factory,
        health,
        trace,
        (settings.mode, settings.voice, settings.lab, settings.reply_lead_ms),
    )
    .await
}

async fn observe(
    client: &WyomingClient,
    events: &mut EventStream,
    factory: &LiveCallFactory<Provider>,
    health: &Health,
    trace: &mut Trace,
    options: (LiveAuthMode, String, bool, u16),
) -> Result<(), Error> {
    let (mode, voice, lab, lead_ms) = options;
    let mut sequence = 0_u64;
    loop {
        let event = events.recv().await?.ok_or(Error::Wyoming)?;
        let Some(run) = event.run else {
            continue;
        };
        sequence += 1;
        let id = format!("run-{sequence}");
        trace.record("wyoming.run_started", Some(&id), json!({}))?;
        trace.record("run.started", Some(&id), json!({}))?;
        health.state.send_modify(|state| state.live = "connecting");
        let peer = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now())
            .await
            .map_err(|_| Error::Unavailable)?;
        let options = SessionOptions {
            model: match mode {
                LiveAuthMode::Chatgpt => "gpt-live-1-codex",
                LiveAuthMode::ApiKey => "gpt-live-1",
            }
            .into(),
            voice: voice.clone(),
            instructions: "Client audio bridge".into(),
            session_id: SessionId::new(id.clone()).map_err(|_| Error::Input)?,
            thread_id: id.clone(),
        };
        let mut call = factory
            .create(peer, options, Instant::now() + Duration::from_secs(5))
            .await?;
        health.state.send_modify(|state| state.live = "connected");
        let result = journey::run(client, events, run, &mut call, trace, &id, (lab, lead_ms)).await;
        call.close();
        health
            .state
            .send_modify(|state| state.live = if result.is_ok() { "idle" } else { "error" });
        result?;
        publish("bridge.run_completed", Some(&id))?;
    }
}

fn control() -> Result<oneshot::Receiver<bool>, Error> {
    let (send, receive) = oneshot::channel();
    // Tokio's blocking stdin cannot be cancelled on an operational failure.
    // An OS thread owns no async resource and cannot delay process exit.
    std::thread::Builder::new()
        .name("bridge-control".into())
        .spawn(move || {
            let _ = send.send(matches!(std::io::stdin().read(&mut [0]), Ok(0)));
        })
        .map_err(|_| Error::Control)?;
    Ok(receive)
}

fn publish(event: &str, run: Option<&str>) -> Result<(), Error> {
    let mut value = json!({"event":event});
    if let Some(id) = run {
        value["run_id"] = id.into();
    }
    writeln!(std::io::stdout(), "{value}")
        .and_then(|()| std::io::stdout().flush())
        .map_err(|_| Error::Control)
}

async fn played(events: &mut EventStream) -> Result<(), Error> {
    loop {
        let event = events.recv().await?.ok_or(Error::Wyoming)?;
        match event.event {
            WyomingEvent::Played(_) => return Ok(()),
            WyomingEvent::RunPipeline(_) => return Err(Error::Wyoming),
            _ => (),
        }
    }
}
