//! One Live call owner drives WebRTC while Wyoming owns its bounded wire queues.
mod bridge_budget;
mod bridge_delegation;
mod bridge_lifecycle;
mod bridge_pacing;
mod bridge_relink;
mod bridge_screen;
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
    call::{LiveAuthMode, LiveCall, LiveCallFactory, SessionOptions},
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
    let mut screen = bridge_screen::Screen::bind(settings.screen.as_ref()).await?;
    let health = Health::bind(settings.health).await?;
    screen.supervise_actions(health.state.subscribe());
    trace.record(
        "bridge.started",
        None,
        json!({"health_addr":health.address.to_string()}),
    )?;
    if let Some(address) = screen.address() {
        trace.record("screen.listening", None, json!({"address":address.to_string()}))?;
    }
    let mut shutdown = control()?;
    let work = async {
        let mut client = tokio::select! {
            result = WyomingClient::connect(settings.wyoming, ClientOptions::default()) => result?,
            eof = &mut shutdown => return match eof { Ok(true) => Ok(()), _ => Err(Error::Control) },
        };
        // Keep the receiver alive until the supervisor has closed. Cancelling
        // connected drops its pending probe, but a final info/mic event can
        // still arrive before Close; dropping the receiver turns that into a
        // terminal capacity failure and races otherwise successful EOF cleanup.
        let mut events = client.events()?;
        let result = tokio::select! {
            result = connected(&client, &mut events, settings, &health, &mut trace, &screen) => result,
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
    let cleanup = screen.close().await;
    result.and(report).and(cleanup)
}

async fn connected(
    client: &WyomingClient,
    events: &mut EventStream,
    settings: Settings,
    health: &Health,
    trace: &mut Trace,
    screen: &bridge_screen::Screen,
) -> Result<(), Error> {
    client.run_satellite().await?;
    client.describe().await?;
    health.state.send_modify(|state| state.wyoming = "connected");
    trace.record("wyoming.connected", Some("connection-1"), json!({}))?;
    publish("bridge.ready", None)?;
    let credential = settings.credential.clone();
    let factory = LiveCallFactory::new(&settings.live_url, settings.mode, credential)?;
    observe(client, events, &factory, health, trace, &settings, screen).await
}

async fn observe(
    client: &WyomingClient,
    events: &mut EventStream,
    factory: &LiveCallFactory<Provider>,
    health: &Health,
    trace: &mut Trace,
    options: &Settings,
    screen: &bridge_screen::Screen,
) -> Result<(), Error> {
    let mut conversation = bridge_lifecycle::Conversation::new(options.session_idle_seconds);
    let mut delegation = bridge_delegation::Host::new(options);
    let mut sequence = 0_u64;
    loop {
        let run = conversation.wake(client, events, health, trace).await?;
        sequence += 1;
        let id = format!("run-{sequence}");
        trace.record("wyoming.run_started", Some(&id), json!({}))?;
        trace.record("run.started", Some(&id), json!({}))?;
        if health.state.borrow().relink_required {
            trace.record("run.refused", Some(&id), json!({"reason":"relink_required"}))?;
            continue;
        }
        screen.publish(echo_screen_contract::UiState::Listening)?;
        let result = turn(
            client,
            events,
            run,
            factory,
            &mut conversation,
            trace,
            (health, options, screen, &id, &mut delegation),
        )
        .await;
        if result.is_err() {
            health.state.send_modify(|state| state.live = "error");
        }
        let Some((sent, reusable)) = result? else { continue };
        health.state.send_modify(|state| state.live = "idle");
        if reusable {
            conversation.completed(sent);
        } else {
            conversation.close("cut", trace)?;
        }
        screen.ready()?;
        publish("bridge.run_completed", Some(&id))?;
    }
}

async fn turn(
    client: &WyomingClient,
    events: &mut EventStream,
    run: echo_wyoming::Run,
    factory: &LiveCallFactory<Provider>,
    conversation: &mut bridge_lifecycle::Conversation,
    trace: &mut Trace,
    context: (
        &Health,
        &Settings,
        &bridge_screen::Screen,
        &str,
        &mut bridge_delegation::Host,
    ),
) -> Result<Option<(u64, bool)>, Error> {
    let (health, options, screen, id, delegation) = context;
    if conversation.call.is_none() {
        delegation.new_call();
        health.state.send_modify(|state| state.live = "connecting");
        conversation.call = match create(factory, options, id).await {
            Ok(call) => Some(call),
            Err(error) => {
                bridge_relink::failure(error, options.lab, health, trace, screen, id)?;
                return Ok(None);
            }
        };
    }
    health.state.send_modify(|state| state.live = "connected");
    let call = conversation.call.as_mut().ok_or(Error::Unavailable)?;
    let result = journey::run(
        client,
        events,
        run,
        call,
        trace,
        id,
        (
            options.lab,
            options.reply_lead_ms,
            conversation.sent,
            screen,
            &mut *delegation,
        ),
    )
    .await;
    match result {
        Ok(outcome) => Ok(Some(outcome)),
        Err(error) => {
            delegation.abandon();
            conversation.close("failed", trace)?;
            bridge_relink::failure(error, options.lab, health, trace, screen, id)?;
            Ok(None)
        }
    }
}

async fn create(factory: &LiveCallFactory<Provider>, settings: &Settings, id: &str) -> Result<LiveCall, Error> {
    let peer = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now())
        .await
        .map_err(|_| Error::Unavailable)?;
    let options = SessionOptions {
        model: match settings.mode {
            LiveAuthMode::Chatgpt => "gpt-live-1-codex",
            LiveAuthMode::ApiKey => "gpt-live-1",
        }
        .into(),
        voice: settings.voice.clone(),
        instructions: "Client audio bridge".into(),
        session_id: SessionId::new(id.to_owned()).map_err(|_| Error::Input)?,
        thread_id: id.to_owned(),
    };
    factory
        .create(peer, options, Instant::now() + Duration::from_secs(5))
        .await
        .map_err(Error::from)
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
