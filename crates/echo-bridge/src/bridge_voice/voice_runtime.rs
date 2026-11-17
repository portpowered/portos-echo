//! Presentation lifetime encloses authenticated device and conversation cleanup.
use super::{HostOptions, ServeOptions, serve, voice_view::View, with_device};
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
}

/// Compose the Codex voice service with the existing screen, health and trace
/// owners. Authentication precedes device readiness. Only negotiated media can
/// publish Connected; played settlement precedes Ready. All device/call/process
/// cleanup finishes while presentation remains owned.
///
/// This library entry point does not select the executable backend or load
/// tools. The service monitors the nominated operator kill file; the caller supplies a
/// peer factory. No legacy call or credential path is used here.
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
            let _ = screen.close().await;
            return Err(error);
        }
    };
    screen.supervise_actions(health.state.subscribe());
    let mut view = View::new(&screen, &health, &mut trace);
    let result = work(options, connector, &mut peer, cancelled, &mut view).await;
    health.close().await;
    let cleanup = screen.close().await;
    result.and(cleanup)
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
            serve(client, io, options.voice, peer, stopped(stopping), |notice| {
                view.observe(notice)
            })
            .await
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
