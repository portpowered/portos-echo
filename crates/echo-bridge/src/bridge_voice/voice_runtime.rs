//! Presentation lifetime encloses authenticated device and conversation cleanup.
use super::{HostOptions, ServeOptions, voice_view::View, with_device};
use crate::{Error, bridge_health::Health, bridge_runtime::bridge_screen::Screen, bridge_trace::Trace};
use echo_config::{LiveMode, ScreenConfig};
use echo_link::LinkConnector;
use echo_media::MediaPeer;
use std::{cell::Cell, future::Future, net::SocketAddr, path::Path};
use tokio::sync::watch;

pub struct RuntimeOptions<'a> {
    pub host: HostOptions<'a>,
    pub voice: ServeOptions<'a>,
    pub screen: Option<&'a ScreenConfig>,
    pub health: SocketAddr,
    pub trace: &'a Path,
    pub delegation: Option<&'a echo_config::BridgeConfig>,
    pub utc_day: std::sync::Arc<dyn Fn() -> u64 + Send + Sync>,
    pub mcp_epoch: Option<u64>,
}

/// Compose the Codex voice service with the existing screen, health and trace
/// owners. Authentication precedes device readiness. Only negotiated media can
/// publish Connected; played settlement precedes Ready. All device/call/process
/// cleanup finishes while presentation remains owned.
///
/// Configured delegation discovers MCP tools after authentication and retains
/// policy, consent and the effect ledger across calls. The service monitors
/// the nominated kill file; the caller supplies a peer factory.
///
/// # Errors
/// Startup, service, publication and cleanup failures use fixed categories.
/// Authorization failures publish the existing relink instruction and keep the
/// screen available until cancellation; no implicit login or restart occurs.
pub async fn execute<C: LinkConnector, P: MediaPeer>(
    options: RuntimeOptions<'_>,
    connector: C,
    mut peer: impl AsyncFnMut() -> Result<P, Error>,
    cancelled: impl Future<Output = ()>,
) -> Result<(), Error> {
    let mut trace = Trace::open(
        options.trace,
        match options.host.mode {
            LiveMode::Chatgpt => "chatgpt",
            LiveMode::ApiKey => "api_key",
        },
    )?;
    let mut screen = Screen::bind(options.screen).await?;
    let health = match Health::bind(options.health).await {
        Ok(health) => health,
        Err(error) => {
            let _ = close_screen(&mut screen, &mut trace).await;
            return Err(error);
        }
    };
    screen.supervise_actions(health.state.subscribe());
    let mut view = View::new(&screen, &health, &mut trace);
    let result = work(options, connector, &mut peer, cancelled, &mut view).await;
    health.close().await;
    let health_receipt = trace.record("health.released", None, serde_json::json!({}));
    // A trace failure must not skip the remaining presentation cleanup.
    let cleanup = close_screen(&mut screen, &mut trace).await;
    result.and(cleanup).and(health_receipt)
}

async fn close_screen(screen: &mut Screen, trace: &mut Trace) -> Result<(), Error> {
    let acquired = screen.address().is_some();
    screen.close().await?;
    if acquired {
        trace.record("screen.released", None, serde_json::json!({}))?;
    }
    Ok(())
}

async fn work<C: LinkConnector, P: MediaPeer>(
    options: RuntimeOptions<'_>,
    connector: C,
    peer: impl AsyncFnMut() -> Result<P, Error>,
    cancelled: impl Future<Output = ()>,
    view: &mut View<'_>,
) -> Result<(), Error> {
    view.startup()?;
    let requested = Cell::new(false);
    let cancelled = async {
        cancelled.await;
        requested.set(true);
    };
    tokio::pin!(cancelled);
    let result = with_device(
        options.host,
        connector,
        cancelled.as_mut(),
        async |io, client, stopping| {
            let mut tools = options.delegation.map(|config| super::voice_tools::Tools::new(config, options.utc_day));
            let setup = async {
                if let (Some(config), Some(tools)) = (options.delegation, &mut tools) {
                    view.open_tools(tools, config, options.mcp_epoch).await?;
                }
                Ok::<_, Error>(())
            };
            let ready = tokio::select! {
                biased;
                () = stopped(stopping.clone()) => Err(Error::Control),
                result = tokio::time::timeout(std::time::Duration::from_secs(15), setup) => result.unwrap_or(Err(Error::Deadline)),
            };
            let setup_trace = view.setup_trace();
            let result = match ready {
                Ok(()) => super::voice_admission::service(client, io, options.voice, peer, stopped(stopping), |notice| {
                view.observe(notice)
                }, (tools.as_mut(), Some(setup_trace), options.delegation.is_some_and(|config| config.wake_earcon))).await,
                Err(error) => Err(error),
            };
            if let Some(tools) = &mut tools { tools.close().await; }
            result
        },
    )
    .await;
    // with_device has now awaited call/media, device and process shutdown.
    match result {
        Err(Error::Control) if requested.get() => {
            view.closed("shutdown")?;
            Ok(())
        }
        Err(error) => {
            view.failed(&error).await?;
            if error == Error::Authorization && !requested.get() {
                cancelled.await;
            }
            Err(error)
        }
        Ok(()) => view.closed("stopped"),
    }
}

async fn stopped(mut stopping: watch::Receiver<bool>) {
    loop {
        if *stopping.borrow_and_update() || stopping.changed().await.is_err() {
            return;
        }
    }
}
