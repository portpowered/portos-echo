//! Executable admission for configured calls with explicit local-fake routing.
use super::Configured;
use crate::bridge_settings::{
    SettingsHost,
    settings_process::{self, ProcessImage, ReplacementGate},
};
use crate::{Error, bridge_input::Settings, bridge_runtime::Control};
use echo_link::LinkConnector;
use echo_media::{MediaPeer, Peer, PeerConfig};
use std::cell::Cell;

/// Own runtime destruction and process replacement for the configured CLI.
/// The original active owner and accepted pending edits survive a failed exec.
///
/// # Errors
/// Returns fixed admission, runtime, control and cleanup failures.
pub fn execute_process(settings: Settings) -> Result<(), Error> {
    let image = ProcessImage::capture();
    let address = settings.wyoming;
    let media = media_config(settings.model_url.is_some());
    let kill = settings
        .config
        .as_ref()
        .ok_or(Error::Input)?
        .audit
        .log_dir
        .join("bridge-kill");
    // Local fake routes stay on loopback; a real run measures its clock against the network.
    let local = settings.model_url.is_some();
    let mut configured = prepare(settings)?;
    if !local {
        configured = configured.with_default_time_servers();
    }
    let mut retained = None;
    loop {
        let completion = process_generation(&mut configured, address, &media, retained)?;
        if !completion.restart_requested {
            return Ok(());
        }
        let mut host = completion.settings.ok_or(Error::Control)?;
        // process_generation has returned and destroyed its runtime: no socket,
        // device, child, lease or asynchronous control subscriber survives here.
        match settings_process::replacement_gate(Some(&kill))? {
            ReplacementGate::Shutdown => return Ok(()),
            ReplacementGate::Killed => (),
            ReplacementGate::Ready => {
                configured.replacement_attempt()?;
                match image.replace() {
                    Ok(never) => match never {},
                    Err(_) => configured.replacement_failed(&mut host)?,
                }
            }
        }
        retained = Some(host);
    }
}

fn process_generation(
    configured: &mut Configured,
    address: std::net::SocketAddr,
    media: &PeerConfig,
    retained: Option<Box<SettingsHost>>,
) -> Result<super::RuntimeCompletion, Error> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| Error::Unavailable)?;
    runtime.block_on(async {
        let shutdown = settings_process::shutdown()?;
        let clean_stop = Cell::new(true);
        let cancelled = async { clean_stop.set(matches!(shutdown.await, Ok(true))) };
        let connector = echo_wyoming::adapter::connector(address, echo_wyoming::ClientOptions::default());
        let result = configured
            .execute_generation(
                connector,
                async || {
                    Box::pin(Peer::create(media.clone()))
                        .await
                        .map_err(|_| Error::Unavailable)
                },
                vec![],
                cancelled,
                retained,
            )
            .await;
        if clean_stop.get() { result } else { Err(Error::Control) }
    })
}

pub(crate) async fn execute<C: LinkConnector>(settings: Settings, control: Control, connector: C) -> Result<(), Error> {
    let local = settings.model_url.is_some();
    let settings_witness = settings.sim_epoch.is_some();
    let configured = prepare(settings)?.with_clock(control.utc_day);
    let media = media_config(local);
    let clean_stop = Cell::new(true);
    let cancelled = async {
        clean_stop.set(matches!(control.shutdown.await, Ok(true)));
    };
    let peer = async || {
        Box::pin(Peer::create(media.clone()))
            .await
            .map_err(|_| Error::Unavailable)
    };
    // An explicit loopback epoch also drives the paired grant journey.
    // The controlled owner cannot replace the process, so refuse changes that require it.
    let result = if settings_witness {
        configured
            .without_settings_restart()
            .execute_with_settings(connector, peer, vec![], cancelled)
            .await
            .map(|_| ())
    } else {
        configured.execute(connector, peer, vec![], cancelled).await
    };
    if clean_stop.get() { result } else { Err(Error::Control) }
}

// Keep the single offline base/overlay read across executable admission. This
// prepares ownership without reopening the base or pending overlay.
pub(super) fn prepare(mut settings: Settings) -> Result<Configured, Error> {
    if settings.lab
        || (settings.sim_clock && settings.model_url.is_none())
        || (settings.sim_epoch.is_some() && !settings.sim_clock)
    {
        return Err(Error::Input);
    }
    let config = settings.config.take().ok_or(Error::Input)?;
    if settings
        .startup
        .as_ref()
        .is_some_and(|startup| startup.config != config)
    {
        return Err(Error::Input);
    }
    let configured = Configured::admit(config, settings.trace, settings.health, settings.runner_skills)?;
    let configured = match settings.startup.take() {
        Some(startup) => configured.with_effective(startup),
        None => configured.with_settings_warning(settings.settings_warning),
    };
    let configured = match settings.model_url {
        Some(model) => configured
            .with_routes(crate::bridge_operator::LocalRoutes::new(&settings.live_url, &model)?)
            .with_mcp_epoch(settings.sim_epoch),
        None => configured,
    };
    Ok(configured)
}

fn media_config(local: bool) -> PeerConfig {
    if local {
        PeerConfig {
            interface_allowlist: vec![std::net::Ipv4Addr::LOCALHOST.into()],
            allow_loopback: true,
            ..PeerConfig::default()
        }
    } else {
        PeerConfig::default()
    }
}
