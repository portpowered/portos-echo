//! One owner retains a healthy call between wakes and drives its idle timers.
use crate::{Error, bridge_health::Health, bridge_trace::Trace};
use echo_contracts::live::LiveEvent;
use echo_live::call::{CallError, LiveCall};
use echo_wyoming::{EventStream, Run, WyomingClient};
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

    pub async fn wake(
        &mut self,
        client: &WyomingClient,
        events: &mut EventStream,
        health: &Health,
        trace: &mut Trace,
    ) -> Result<Run, Error> {
        let mut probe = tokio::time::interval(Duration::from_millis(250));
        probe.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            // Expiry wins at equality, including when a wake is already queued.
            if self.expired() {
                self.close("idle", trace)?;
            }
            let event = tokio::select! {
                event = events.recv() => event?,
                _ = probe.tick() => {
                    observe_link(client, health, trace).await?;
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
            if let Some(run) = event.ok_or(Error::Wyoming)?.run {
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

async fn observe_link(client: &WyomingClient, health: &Health, trace: &mut Trace) -> Result<(), Error> {
    let connected = client.describe().await.is_ok();
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

#[cfg(test)]
mod tests {
    use super::*;

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
