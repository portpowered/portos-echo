//! Executable admission for configured calls with explicit local-fake routing.
use super::Configured;
use crate::{Error, bridge_input::Settings, bridge_runtime::Control};
use echo_link::LinkConnector;
use echo_media::{MediaPeer, Peer, PeerConfig};
use std::cell::Cell;

pub(crate) async fn execute<C: LinkConnector>(settings: Settings, control: Control, connector: C) -> Result<(), Error> {
    let local = settings.model_url.is_some();
    let configured = prepare(settings)?.with_clock(control.utc_day);
    let media = media_config(local);
    let clean_stop = Cell::new(true);
    let cancelled = async {
        clean_stop.set(matches!(control.shutdown.await, Ok(true)));
    };
    let result = configured
        .execute(
            connector,
            async || {
                Box::pin(Peer::create(media.clone()))
                    .await
                    .map_err(|_| Error::Unavailable)
            },
            vec![],
            cancelled,
        )
        .await;
    if clean_stop.get() { result } else { Err(Error::Control) }
}

// Keep the single offline base/overlay read across executable admission. This
// prepares ownership only; execute still uses the inactive settings surface.
pub(super) fn prepare(mut settings: Settings) -> Result<Configured, Error> {
    if settings.lab
        || (settings.sim_clock && settings.model_url.is_none())
        || (settings.sim_epoch.is_some() && !settings.sim_clock)
    {
        return Err(Error::Input);
    }
    let config = settings.config.take().ok_or(Error::Input)?;
    let configured = match settings.startup.take() {
        Some(startup) if startup.config != config => return Err(Error::Input),
        Some(startup) => Configured::from_effective(*startup, settings.trace, settings.health)?,
        None => {
            Configured::new(config, settings.trace, settings.health)?.with_settings_warning(settings.settings_warning)
        }
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
