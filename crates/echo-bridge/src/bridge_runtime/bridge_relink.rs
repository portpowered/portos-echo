//! A credential failure latches refusal until explicit sign-in and restart.
use super::bridge_screen::Screen;
use crate::{Error, bridge_health::Health, bridge_trace::Trace};
use serde_json::json;

pub(super) fn failure(
    error: Error,
    lab: bool,
    health: &Health,
    trace: &mut Trace,
    screen: &Screen,
    id: &str,
) -> Result<(), Error> {
    if let Error::Mcp(reason) = &error {
        trace.record(
            "mcp.run.failed",
            Some(id),
            json!({"reason":crate::bridge_mcp::category(reason)}),
        )?;
        return Ok(());
    }
    health.state.send_modify(|state| state.live = "error");
    // The synthetic lab caller retains its existing fatal-error protocol.
    if lab
        || !matches!(
            error,
            Error::Authorization | Error::Unavailable | Error::Deadline | Error::Decode
        )
    {
        return Err(error);
    }
    let relink = error == Error::Authorization;
    health.state.send_modify(|state| state.relink_required = relink);
    trace.record(
        if relink { "auth.relink_required" } else { "run.failed" },
        Some(id),
        json!({"reason":error.to_string()}),
    )?;
    screen.publish(super::bridge_screen::failure_state(relink))
}
