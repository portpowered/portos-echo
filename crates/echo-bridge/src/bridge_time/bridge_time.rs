//! The time the bridge tells the model, kept right when the system clock is not.
//!
//! On the Echo nothing sets the system clock: the device firewall lets only the
//! bridge's group out, so Android's own network time never arrives. The bridge
//! therefore measures the offset itself (`sntp`), at startup and then every
//! `clock.interval_seconds`, and [`now`] is the system clock moved by it while
//! the two differ by more than [`THRESHOLD_MS`]. The measured time is carried
//! on the monotonic clock, so once root sets the system clock (`echo-bridge
//! clock sync`) the two agree and the system clock is used again at once.
//!
//! Each measurement appends `clock.offset` to the trace: `synced`, `hosts`
//! (the servers that agreed, or answered when unsynced), `servers` (configured)
//! and `corrected` (whether [`now`] differs from the system clock), with
//! `offset_ms` (network time minus system clock) when synced and a fixed
//! `reason` (`unreachable`, `disagreement`, `timeout`) when not. A run of
//! unsynced measurements is traced once. No server name or address is traced.
//! A failed measurement keeps the last good one and never fails the bridge.
pub mod clock_command;
pub(crate) mod sntp;
#[cfg(test)]
mod sntp_tests;
#[cfg(test)]
mod time_tests;
use crate::bridge_trace::Trace;
use serde_json::json;
use sntp::Outcome;
use std::{
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// A system clock this close to the measured time is used as it is.
const THRESHOLD_MS: i64 = 2000;
const QUERY_TIMEOUT: Duration = Duration::from_millis(1500);
/// Startup waits this long for the first measurement, so the first call has it.
const STARTUP: Duration = Duration::from_secs(4);
const RETRY: Duration = Duration::from_secs(60);

/// Network time when it was measured, carried forward on the monotonic clock.
static ANCHOR: Mutex<Option<(i64, Instant)>> = Mutex::new(None);

tokio::task_local! {
    /// Model-facing UTC anchor owned by a guarded, fixed-epoch simulation.
    pub(crate) static SIM_TIME: Option<(SystemTime, tokio::time::Instant)>;
}

fn reference_ms() -> Option<i64> {
    let (measured, at) = (*ANCHOR.lock().ok()?)?;
    measured.checked_add(i64::try_from(at.elapsed().as_millis()).ok()?)
}

/// The current time for everything the model is told.
#[must_use]
#[allow(clippy::disallowed_methods, reason = "Existing host time")]
pub fn now() -> SystemTime {
    if let Ok(Some((epoch, start))) = SIM_TIME.try_with(|anchor| *anchor) {
        return epoch.checked_add(start.elapsed()).unwrap_or(epoch);
    }
    corrected(SystemTime::now(), reference_ms())
}

fn differs(system: SystemTime, reference_ms: i64) -> bool {
    let system_ms = system
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_millis()).ok());
    system_ms.is_none_or(|system_ms| system_ms.abs_diff(reference_ms) > THRESHOLD_MS.unsigned_abs())
}

fn corrected(system: SystemTime, reference_ms: Option<i64>) -> SystemTime {
    match reference_ms.map(|reference| (reference, u64::try_from(reference))) {
        Some((signed, Ok(reference))) if differs(system, signed) => UNIX_EPOCH + Duration::from_millis(reference),
        _ => system,
    }
}

/// Stops the periodic measurement when dropped.
pub(crate) struct Watch(Option<tokio::task::JoinHandle<()>>);

impl Watch {
    pub(crate) async fn stop(mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}

struct Reporter {
    trace: Option<Trace>,
    servers: usize,
    unsynced: bool,
}

impl Reporter {
    /// Adopt a good measurement, trace it, and say whether it was one.
    #[allow(clippy::disallowed_methods, reason = "Existing host time")]
    fn observe(&mut self, outcome: Outcome, startup: bool) -> bool {
        let fields = match outcome {
            Outcome::Synced { offset_ms, hosts } => {
                if let Ok(mut anchor) = ANCHOR.lock() {
                    *anchor = Some((sntp::system_ms().saturating_add(offset_ms), Instant::now()));
                }
                self.unsynced = false;
                json!({"synced":true, "offset_ms":offset_ms, "hosts":hosts})
            }
            Outcome::Unsynced { .. } if self.unsynced => return false,
            Outcome::Unsynced { reason, answered } => {
                self.unsynced = true;
                json!({"synced":false, "reason":reason, "hosts":answered})
            }
        };
        let corrected = reference_ms().is_some_and(|reference| differs(SystemTime::now(), reference));
        if let (Some(trace), serde_json::Value::Object(mut fields)) = (&mut self.trace, fields) {
            fields.insert("servers".into(), self.servers.into());
            fields.insert("corrected".into(), corrected.into());
            // The trace is evidence only: a full disk must not take the bridge down.
            if startup || !self.unsynced {
                let _ = trace.record("clock.offset", None, fields.into());
            } else {
                let _ = trace.buffer_clock_offset(fields.into());
            }
        }
        !self.unsynced
    }
}

/// Measure once, bounded, then keep measuring until the returned watch is dropped.
/// No servers: nothing is measured, traced or changed.
pub(crate) async fn start(servers: Vec<String>, interval: Duration, trace: Option<Trace>) -> Watch {
    if servers.is_empty() {
        return Watch(None);
    }
    let mut reporter = Reporter {
        trace,
        servers: servers.len(),
        unsynced: false,
    };
    let first = tokio::time::timeout(STARTUP, sntp::measure(&servers, QUERY_TIMEOUT))
        .await
        .unwrap_or(Outcome::Unsynced {
            reason: "timeout",
            answered: 0,
        });
    let mut synced = reporter.observe(first, true);
    Watch(Some(tokio::spawn(async move {
        loop {
            tokio::time::sleep(if synced { interval } else { RETRY.min(interval) }).await;
            synced = reporter.observe(sntp::measure(&servers, QUERY_TIMEOUT).await, false);
        }
    })))
}
