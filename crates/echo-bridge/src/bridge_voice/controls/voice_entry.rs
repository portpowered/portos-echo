//! Executable admission for configured calls with explicit local-fake routing.
use super::Configured;
use crate::{Error, bridge_input::Settings, bridge_runtime::Control};
use echo_link::LinkConnector;
use echo_media::{MediaPeer, Peer, PeerConfig};
use std::cell::Cell;

pub(crate) async fn execute<C: LinkConnector>(
    mut settings: Settings,
    control: Control,
    connector: C,
) -> Result<(), Error> {
    if settings.sim_epoch.is_some() || settings.lab || (settings.sim_clock && settings.model_url.is_none()) {
        return Err(Error::Input);
    }
    let config = settings.config.take().ok_or(Error::Input)?;
    let has_tools = !config.mcp.server.is_empty();
    let configured = Configured::new(config, settings.trace, settings.health)?;
    let configured = match settings.model_url {
        Some(model) => configured.with_routes(crate::bridge_operator::LocalRoutes::new(&settings.live_url, &model)?),
        None => configured,
    };
    // Do not silently discard configured tools while their dispatcher is being
    // integrated. Neither the legacy agent nor an unapproved tool host may run.
    if has_tools {
        return Err(Error::Input);
    }
    let clean_stop = Cell::new(true);
    let cancelled = async {
        clean_stop.set(matches!(control.shutdown.await, Ok(true)));
    };
    let result = configured
        .execute(
            connector,
            async || {
                Peer::create(PeerConfig::default())
                    .await
                    .map_err(|_| Error::Unavailable)
            },
            vec![],
            cancelled,
        )
        .await;
    if clean_stop.get() { result } else { Err(Error::Control) }
}
