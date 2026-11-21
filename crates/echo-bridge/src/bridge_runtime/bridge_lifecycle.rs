//! One owner retains a healthy call between wakes and drives its idle timers.
use crate::{Error, bridge_health::Health, bridge_trace::Trace};
use echo_contracts::live::LiveEvent;
use echo_link::{EchoLink, LinkEvent, LinkEvents};
use echo_live::call::{CallError, LiveCall};
use serde_json::json;
use std::time::Duration;
use tokio::time::Instant;

pub(super) struct Conversation {
    pub call: Option<LiveCall>,
    pub sent: u64,
    expires: Option<Instant>,
    idle: Duration,
}

impl Conversation {
    pub fn new(seconds: u64) -> Self {
        Self {
            call: None,
            sent: 0,
            expires: None,
            idle: Duration::from_secs(seconds),
        }
    }

    pub fn completed(&mut self, sent: u64) {
        self.sent += sent;
        // The config accepts every u64. An unrepresentable deadline cannot
        // expire during this process's lifetime; do not panic or narrow it.
        self.expires = Instant::now().checked_add(self.idle);
    }

    fn expired(&self) -> bool {
        self.expires.is_some_and(|deadline| Instant::now() >= deadline)
    }

    pub fn close(&mut self, reason: &str, trace: &mut Trace) -> Result<(), Error> {
        if let Some(mut call) = self.call.take() {
            call.close();
            trace.record("live.closed", None, json!({"reason":reason}))?;
        }
        self.sent = 0;
        self.expires = None;
        Ok(())
    }

    pub async fn wake<L: EchoLink>(
        &mut self,
        client: &L,
        events: &mut L::Events,
        health: &Health,
        trace: &mut Trace,
        kill: &super::bridge_kill::Kill<'_>,
    ) -> Result<<L::Events as LinkEvents>::Run, Error> {
        let policy = echo_wyoming::ClientOptions::default();
        let mut cadence = KillCadence(Instant::now());
        let mut backoff = policy.reconnect_initial;
        let probe = probe_link(client, Duration::ZERO);
        tokio::pin!(probe);
        loop {
            if cadence.due(Instant::now()) && kill.sync(trace).await? {
                self.close("kill", trace)?;
            }
            // Expiry wins at equality, including when a wake is already queued.
            if self.expired() {
                self.close("idle", trace)?;
            }
            let event = tokio::select! {
                () = tokio::time::sleep_until(cadence.0) => continue,
                event = events.recv() => event?,
                connected = &mut probe => {
                    if connected {
                        trace.record("wyoming.probe_ok", Some("connection-1"), json!({}))?;
                    }
                    observe_link(connected, health, trace)?;
                    backoff = probe_backoff(connected, backoff, &policy);
                    probe.set(probe_link(client, backoff));
                    continue;
                }
                result = self.service() => {
                    if result.is_err() {
                        self.close("unavailable", trace)?;
                        health.state.send_modify(|state| state.live = "error");
                    }
                    continue;
                }
            };
            let event = event.ok_or(Error::Wyoming)?;
            let muted = kill.screen.observe(&event, trace)?;
            if let Some(run) = select_wake(Some(event))?.filter(|_| !muted) {
                if self.expired() {
                    self.close("idle", trace)?;
                }
                if let Some(call) = &mut self.call {
                    while call.pop_audio().is_some() {}
                }
                self.expires = None;
                return Ok(run);
            }
        }
    }
    async fn service(&mut self) -> Result<(), CallError> {
        let Some(call) = &mut self.call else {
            return std::future::pending().await;
        };
        let poll = Instant::now() + Duration::from_millis(20);
        let poll = poll.min(self.expires.unwrap_or(poll));
        let event = call.receive_event(poll.into_std()).await;
        // Idle audio belongs to the previous turn, never the next wake.
        while call.pop_audio().is_some() {}
        match event {
            Ok(LiveEvent::Error { .. }) => Err(CallError::Unavailable),
            Ok(_) | Err(CallError::Deadline) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

// The due decision precedes select, so ready events cannot starve checks.
// A late check schedules the next interval without a catch-up syscall burst.
struct KillCadence(Instant);
impl KillCadence {
    fn due(&mut self, now: Instant) -> bool {
        if now < self.0 {
            return false;
        }
        self.0 = now + Duration::from_millis(20);
        true
    }
}

fn probe_backoff(connected: bool, previous: Duration, policy: &echo_wyoming::ClientOptions) -> Duration {
    if connected {
        policy.reconnect_initial
    } else {
        previous.saturating_mul(2).min(policy.reconnect_cap)
    }
}

async fn probe_link<L: EchoLink>(client: &L, delay: Duration) -> bool {
    tokio::time::sleep(delay).await;
    client.probe().await.is_ok()
}

fn observe_link(connected: bool, health: &Health, trace: &mut Trace) -> Result<(), Error> {
    let previous = health.state.borrow().wyoming;
    let state = if connected { "connected" } else { "reconnecting" };
    if previous != state {
        health.state.send_modify(|links| links.wyoming = state);
        trace.record(
            if connected {
                "wyoming.connected"
            } else {
                "wyoming.reconnecting"
            },
            Some("connection-1"),
            if connected {
                json!({})
            } else {
                json!({"reason":"disconnected"})
            },
        )?;
    }
    Ok(())
}

// Shared by idle observation and the in-memory journey witness.
pub(super) fn select_wake<R>(event: Option<LinkEvent<R>>) -> Result<Option<R>, Error> {
    match event.ok_or(Error::Wyoming)? {
        LinkEvent::Wake(run) => Ok(Some(run)),
        LinkEvent::Played
        | LinkEvent::Other
        | LinkEvent::Mute(_)
        | LinkEvent::MicrophoneDropped(_)
        | LinkEvent::StaleRemoteCapture
        | LinkEvent::RemoteCapture { .. } => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn bridge_lifecycle_kill_cadence_ignores_ready_event_count() {
        let start = Instant::now();
        let mut cadence = KillCadence(start);
        assert!(cadence.due(start));
        for _ in 0..10_000 {
            assert!(!cadence.due(Instant::now()));
        }
        tokio::time::advance(Duration::from_millis(19)).await;
        assert!(!cadence.due(Instant::now()));
        assert_eq!(cadence.0, start + Duration::from_millis(20));
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(cadence.due(Instant::now()));
        assert!(!cadence.due(Instant::now()));
        assert_eq!(cadence.0, start + Duration::from_millis(40));
    }

    #[test]
    fn bridge_lifecycle_probe_backoff_uses_client_policy_and_resets_on_success() {
        let policy = echo_wyoming::ClientOptions::default();
        let mut next = policy.reconnect_initial;
        let mut delays = vec![next.as_millis()];
        for _ in 0..6 {
            next = probe_backoff(false, next, &policy);
            delays.push(next.as_millis());
        }
        assert_eq!(delays, [100, 200, 400, 800, 1600, 2000, 2000]);
        assert_eq!(probe_backoff(true, next, &policy), Duration::from_millis(100));
        let custom = echo_wyoming::ClientOptions {
            reconnect_initial: Duration::from_millis(7),
            reconnect_cap: Duration::from_millis(9),
            ..policy
        };
        assert_eq!(
            probe_backoff(false, Duration::from_millis(7), &custom),
            Duration::from_millis(9)
        );
        assert_eq!(
            probe_backoff(true, Duration::from_millis(9), &custom),
            Duration::from_millis(7)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn bridge_lifecycle_zero_expires_immediately_and_unrepresentable_idle_never_panics() {
        let mut immediate = Conversation::new(0);
        immediate.completed(0);
        assert!(immediate.expired());
        let mut distant = Conversation::new(u64::MAX);
        distant.completed(32);
        assert_eq!(distant.expires, None);
        assert!(!distant.expired());
        assert_eq!(distant.sent, 32);
    }

    #[tokio::test(start_paused = true)]
    async fn bridge_lifecycle_deadline_resets_after_each_completed_run_and_expires_at_equality() {
        let mut session = Conversation::new(120);
        assert!(!session.expired());
        session.completed(32);
        assert_eq!(session.sent, 32);
        tokio::time::advance(Duration::from_secs(119)).await;
        assert!(!session.expired());
        session.completed(32);
        assert_eq!(session.sent, 64);
        tokio::time::advance(Duration::from_secs(119)).await;
        assert!(!session.expired());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(session.expired());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace");
        let mut trace = Trace::open(&path, "chatgpt").unwrap();
        session.close("idle", &mut trace).unwrap();
        assert_eq!(session.sent, 0);
        assert!(!session.expired());
        assert_eq!(std::fs::read(path).unwrap(), b"");
    }
}
