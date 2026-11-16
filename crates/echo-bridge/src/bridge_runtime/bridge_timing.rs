//! Run budgets include the configured task window; control and cleanup stay short.
use crate::bridge_input::Settings;
use std::time::Duration;

pub(super) const CONTROL: Duration = Duration::from_secs(5);

pub(super) const RESPONSE: Duration = Duration::from_secs(10);

pub(super) fn task(settings: &Settings) -> Duration {
    settings.delegation.as_ref().map_or(Duration::ZERO, |(agent, _)| {
        Duration::from_secs(agent.task_deadline_seconds)
    })
}

pub(super) fn playback(settings: &Settings) -> Duration {
    // The owner enforces production/cut/Played limits. This last-resort request
    // ceiling covers every delegation the existing bounded call can admit.
    RESPONSE
        .saturating_add(task(settings).saturating_mul(super::bridge_delegation::MAX_DELEGATIONS))
        .saturating_add(CONTROL * 3)
}

/// Check the owner response budget without changing its deadline.
pub(super) fn check_deadline(deadline: tokio::time::Instant) -> Result<(), crate::Error> {
    if tokio::time::Instant::now() >= deadline {
        Err(crate::Error::Deadline)
    } else {
        Ok(())
    }
}
