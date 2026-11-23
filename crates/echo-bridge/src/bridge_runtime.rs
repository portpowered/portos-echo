//! One Live call owner drives WebRTC while Wyoming owns its bounded wire queues.
mod bridge_budget;
mod bridge_delegation;
pub(crate) mod bridge_kill;
pub(crate) use bridge_delegation::bridge_muse_work;
mod bridge_lifecycle;
pub(crate) mod bridge_pacing;
mod bridge_refusal;
mod bridge_relink;
pub(crate) mod bridge_screen;
mod bridge_timing;
mod journey;

use crate::{
    Error,
    bridge_health::Health,
    bridge_input::{Provider, Settings},
    bridge_trace::Trace,
};
use echo_contracts::ids::SessionId;
use echo_link::{EchoLink, LinkConnector, LinkEvent, LinkEvents, RunFailure};
use echo_live::{
    LivePeer,
    call::{LiveAuthMode, LiveCall, LiveCallFactory, SessionOptions},
};
use serde_json::json;
use std::{io::Write, time::Duration};
use tokio::sync::oneshot;
use tokio::time::Instant;

/// Host clock and shutdown boundary. The binary uses real UTC and stdin EOF.
pub struct Control {
    pub utc_day: std::sync::Arc<dyn Fn() -> u64 + Send + Sync>,
    pub shutdown: oneshot::Receiver<bool>,
}

/// # Errors
/// All failures use fixed categories. EOF closes owned tasks and sockets.
pub async fn execute_wyoming(settings: Settings) -> Result<(), Error> {
    let local = settings.lab || settings.model_url.is_some();
    let (connector, world) = crate::bridge_world::connector(settings.wyoming.clone(), local)?;
    let result = execute_in(settings, connector, Some(world.clone())).await;
    result.and(world.publish())
}

/// # Errors
/// All failures use fixed categories. EOF closes owned tasks and sockets.
pub async fn execute<C: LinkConnector>(settings: Settings, connector: C) -> Result<(), Error> {
    execute_in(settings, connector, None).await
}

async fn execute_in<C: LinkConnector>(
    settings: Settings,
    connector: C,
    world: Option<std::sync::Arc<crate::bridge_world::Controller>>,
) -> Result<(), Error> {
    let control = Control {
        utc_day: std::sync::Arc::new(utc_day),
        shutdown: crate::bridge_settings::settings_process::shutdown()?,
    };
    execute_with_world(settings, control, connector, world).await
}

/// Select the configured owner while preserving explicit clock/stop control.
/// # Errors
/// The selected owner reports fixed startup, device, call and cleanup errors.
pub async fn execute_with_control<C: LinkConnector>(
    settings: Settings,
    control: Control,
    connector: C,
) -> Result<(), Error> {
    execute_with_world(settings, control, connector, None).await
}

pub(crate) async fn execute_with_world<C: LinkConnector>(
    settings: Settings,
    control: Control,
    connector: C,
    world: Option<std::sync::Arc<crate::bridge_world::Controller>>,
) -> Result<(), Error> {
    if configured(&settings) {
        return Box::pin(crate::bridge_voice::voice_entry::execute(
            settings, control, connector, world,
        ))
        .await;
    }
    Box::pin(execute_controlled_in(settings, control, connector, world)).await
}

/// Whether these inputs select the configured Codex owner.
#[must_use]
pub fn configured(settings: &Settings) -> bool {
    settings.config.is_some()
        && (settings.model_url.is_none()
            || settings
                .config
                .as_ref()
                .is_some_and(|config| config.codex.home.is_some()))
}

fn utc_day() -> u64 {
    echo_core_api::Clock::now(&echo_core_api::SystemClock)
        .wall
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86_400
}

fn validate_sim(settings: &Settings) -> Result<(), Error> {
    if settings
        .config
        .as_ref()
        .is_some_and(|config| config.codex.home.is_some())
        || settings.sim_epoch.is_some() && (!settings.sim_clock || settings.model_url.is_none() || settings.lab)
    {
        return Err(Error::Input);
    }
    if let Some(path) = &settings.kill_path {
        bridge_kill::inspect(path)?;
    }
    Ok(())
}

/// Retained simulator owner with an explicit host clock and stop receiver.
/// Configured Codex runs use `execute_with_control`; keeping this legacy owner
/// separate preserves its Send boundary for existing spawned library callers.
/// # Errors
/// Returns the same fixed operational failures as `execute`.
pub async fn execute_controlled<C: LinkConnector>(
    settings: Settings,
    control: Control,
    connector: C,
) -> Result<(), Error> {
    execute_controlled_in(settings, control, connector, None).await
}

async fn execute_controlled_in<C: LinkConnector>(
    mut settings: Settings,
    control: Control,
    connector: C,
    world: Option<std::sync::Arc<crate::bridge_world::Controller>>,
) -> Result<(), Error> {
    validate_sim(&settings)?;
    let mut delegation = bridge_delegation::Host::new(&settings, control.utc_day);
    let mut trace = sim_trace(&settings, world)?;
    let mut screen = bridge_screen::Screen::bind(settings.screen.as_ref()).await?;
    screen.download_packs(settings.config.as_ref(), &trace);
    screen.settings_warning(settings.settings_warning.as_deref(), &mut trace)?;
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
    let mut shutdown = control.shutdown;
    let mut mcp = mount_mcp(
        settings.mcp.take(),
        settings.sim_epoch,
        &screen,
        &mut delegation,
        &mut trace,
    )
    .await?;
    let work = async {
        let mut client = tokio::select! {
            result = connector.connect() => result?,
            eof = &mut shutdown => return match eof { Ok(true) => Ok(()), _ => Err(Error::Control) },
        };
        // Keep the receiver alive until the supervisor has closed. Cancelling
        // connected drops its pending probe, but a final info/mic event can
        // still arrive before Close; dropping the receiver turns that into a
        // terminal capacity failure and races otherwise successful EOF cleanup.
        let mut events = client.events()?;
        let result = tokio::select! {
            result = connected(&client, &mut events, settings, &health, &mut trace, &screen, &mut delegation) => result,
            eof = &mut shutdown => match eof { Ok(true) => Ok(()), _ => Err(Error::Control) },
        };
        delegation.abandon().await;
        let cleanup = stop_link(&client, &screen).await;
        result.and(cleanup)
    };
    let result = work.await;
    if let Some(mcp) = &mut mcp {
        mcp.close().await;
    }
    let report = result.as_ref().err().map_or(Ok(()), |error| {
        trace.record("bridge.error", None, json!({"reason":error.to_string()}))
    });
    health.close().await;
    let cleanup = screen.close().await;
    let drain = trace.finish();
    result.and(report).and(cleanup).and(drain)
}

fn sim_trace(
    settings: &Settings,
    world: Option<std::sync::Arc<crate::bridge_world::Controller>>,
) -> Result<Trace, Error> {
    let mode = match settings.mode {
        LiveAuthMode::Chatgpt => "chatgpt",
        LiveAuthMode::ApiKey => "api_key",
    };
    let trace = Trace::open_in(
        &settings.trace,
        mode,
        world.unwrap_or_else(crate::bridge_world::Controller::new),
    )?;
    Ok(trace.with_payloads(settings.config.as_ref().is_none_or(|config| config.trace.payloads)))
}

async fn stop_link<L: EchoLink>(client: &L, screen: &bridge_screen::Screen) -> Result<(), Error> {
    let cleanup = client.close().await.map_err(Error::from);
    cleanup.and(screen.presentation.clear_emotion())
}

async fn mount_mcp(
    settings: Option<crate::bridge_mcp::Settings>,
    epoch: Option<u64>,
    screen: &bridge_screen::Screen,
    delegation: &mut bridge_delegation::Host,
    trace: &mut Trace,
) -> Result<Option<crate::bridge_mcp::Runtime>, Error> {
    let Some(settings) = settings else {
        return Ok(None);
    };
    let runtime = crate::bridge_mcp::Runtime::open(settings, screen.presentation.clone(), trace.clone(), epoch).await?;
    delegation.grant(runtime.grant.clone());
    trace.record(
        "mcp.operator.listening",
        None,
        json!({"address":runtime.address.to_string()}),
    )?;
    Ok(Some(runtime))
}

async fn connected<L: EchoLink>(
    client: &L,
    events: &mut L::Events,
    settings: Settings,
    health: &Health,
    trace: &mut Trace,
    screen: &bridge_screen::Screen,
    delegation: &mut bridge_delegation::Host,
) -> Result<(), Error> {
    client.enable_capture(30).await?;
    client.probe().await?;
    health.state.send_modify(|state| state.wyoming = "connected");
    trace.record("wyoming.connected", Some("connection-1"), json!({}))?;
    loop {
        let event = events.recv().await?.ok_or(Error::Wyoming)?;
        screen.observe(&event, trace)?;
        if matches!(event, LinkEvent::Other) {
            break;
        }
    }
    if !screen.muted()? {
        screen.ready()?;
    }
    publish("bridge.ready", None)?;
    let credential = settings.credential.clone();
    let factory = LiveCallFactory::new(&settings.live_url, settings.mode, credential)?;
    observe(client, events, &factory, health, trace, &settings, (screen, delegation)).await
}

async fn observe<L: EchoLink>(
    client: &L,
    events: &mut L::Events,
    factory: &LiveCallFactory<Provider>,
    health: &Health,
    trace: &mut Trace,
    options: &Settings,
    context: (&bridge_screen::Screen, &mut bridge_delegation::Host),
) -> Result<(), Error> {
    let (screen, delegation) = context;
    let kill = bridge_kill::Kill {
        settings: options,
        health,
        screen,
    };
    let mut conversation = bridge_lifecycle::Conversation::new(options.session_idle_seconds);
    let mut sequence = 0_u64;
    loop {
        let run = conversation.wake(client, events, health, trace, &kill).await?;
        let cutoff = bridge_timing::Wait::cutoff(options, Instant::now())?;
        sequence += 1;
        let id = format!("run-{sequence}");
        trace.record("wyoming.run_started", Some(&id), json!({}))?;
        trace.record("run.started", Some(&id), json!({}))?;
        let mut link = bridge_relink::RunLink::new(client);
        link.kill = Some(&kill);
        link.lab = options.lab;
        if kill.sync(trace).await? {
            trace.record("run.refused", Some(&id), json!({"reason":"kill"}))?;
            link.refuse(RunFailure::KillSwitch).await?;
            continue;
        }
        if health.state.borrow().relink_required {
            trace.record("run.refused", Some(&id), json!({"reason":"relink_required"}))?;
            link.refuse(RunFailure::RelinkRequired).await?;
            continue;
        }
        screen.publish(echo_screen_contract::UiState::Listening)?;
        let result = turn(
            &link,
            events,
            run,
            factory,
            &mut conversation,
            trace,
            (health, options, screen, &id, delegation, &kill, cutoff),
        )
        .await;
        if kill.sync(trace).await? {
            result?;
            delegation.abandon().await;
            conversation.close("kill", trace)?;
            continue;
        }
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

async fn turn<L: EchoLink>(
    client: &bridge_relink::RunLink<'_, L>,
    events: &mut L::Events,
    run: <L::Events as LinkEvents>::Run,
    factory: &LiveCallFactory<Provider>,
    conversation: &mut bridge_lifecycle::Conversation,
    trace: &mut Trace,
    context: (
        &Health,
        &Settings,
        &bridge_screen::Screen,
        &str,
        &mut bridge_delegation::Host,
        &bridge_kill::Kill<'_>,
        Instant,
    ),
) -> Result<Option<(u64, bool)>, Error> {
    let (health, options, screen, id, delegation, kill, cutoff) = context;
    if !ensure_call(
        client,
        factory,
        conversation,
        trace,
        (health, options, screen, id, delegation, kill),
    )
    .await?
    {
        return Ok(None);
    }
    let call = connected_call(&mut conversation.call, health, screen)?;
    let result = journey::run(
        client,
        events,
        run,
        call,
        trace,
        (id, cutoff),
        (
            options.lab,
            options.reply_lead_ms,
            conversation.sent,
            screen,
            &mut *delegation,
            kill,
        ),
    )
    .await;
    match result {
        Ok(outcome) => {
            if kill.enabled() {
                client.refuse(RunFailure::KillSwitch).await?;
            }
            Ok(Some(outcome))
        }
        Err(error) => {
            delegation.abandon().await;
            if kill.sync(trace).await? {
                client.refuse(RunFailure::KillSwitch).await?;
                conversation.close("kill", trace)?;
                return Ok(None);
            }
            conversation.close("failed", trace)?;
            client.failure(error, (options.lab, health, trace, screen, id)).await?;
            Ok(None)
        }
    }
}

fn connected_call<'a>(
    call: &'a mut Option<LiveCall>,
    health: &Health,
    screen: &bridge_screen::Screen,
) -> Result<&'a mut LiveCall, Error> {
    let call = call.as_mut().ok_or(Error::Unavailable)?;
    health.state.send_modify(|state| state.live = "connected");
    screen.publish(echo_screen_contract::UiState::Connected)?;
    Ok(call)
}

async fn ensure_call<L: EchoLink>(
    client: &bridge_relink::RunLink<'_, L>,
    factory: &LiveCallFactory<Provider>,
    conversation: &mut bridge_lifecycle::Conversation,
    trace: &mut Trace,
    context: (
        &Health,
        &Settings,
        &bridge_screen::Screen,
        &str,
        &mut bridge_delegation::Host,
        &bridge_kill::Kill<'_>,
    ),
) -> Result<bool, Error> {
    let (health, options, screen, id, delegation, kill) = context;
    if conversation.call.is_none() {
        delegation.new_call().await;
        health.state.send_modify(|state| state.live = "connecting");
        let created = tokio::select! {
            biased;
            () = kill.wait() => {
                client.refuse(RunFailure::KillSwitch).await?;
                return Ok(false);
            },
            result = create(factory, options, id) => result,
        };
        conversation.call = match created {
            Ok(call) => Some(call),
            Err(error) => {
                client.failure(error, (options.lab, health, trace, screen, id)).await?;
                return Ok(false);
            }
        };
    }
    Ok(true)
}

async fn create(factory: &LiveCallFactory<Provider>, settings: &Settings, id: &str) -> Result<LiveCall, Error> {
    let peer = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
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
        .create(peer, options, (Instant::now() + Duration::from_secs(5)).into_std())
        .await
        .map_err(Error::from)
}

pub(crate) fn publish(event: &str, run: Option<&str>) -> Result<(), Error> {
    let mut value = json!({"event":event});
    if let Some(id) = run {
        value["run_id"] = id.into();
    }
    writeln!(std::io::stdout(), "{value}")
        .and_then(|()| std::io::stdout().flush())
        .map_err(|_| Error::Control)
}
