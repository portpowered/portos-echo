//! Hold one runner request across the wake's cutoff, then answer a new wake.
use super::super::RECOVERY_MAX_SECONDS;
use super::{Control, World};
use serde_json::Value;

impl World {
    pub(super) async fn abandon_stuck_turn(&mut self) -> Result<(), String> {
        self.wait_for_call(1).await?;
        self.provider()?.control(Control::Delegate(0)).await?;
        self.wait_until(
            "stalled runner request",
            |world| {
                world
                    .provider
                    .as_ref()
                    .is_some_and(|provider| provider.count("waiting") == 1)
                    && world.count("delegation.created") == 1
            },
            20,
        )
        .await?;
        let run = self
            .trace
            .records
            .iter()
            .find(|row| row["event"] == "run.started")
            .and_then(|row| row["elapsed_ms"].as_u64())
            .ok_or("session: no admitted run clock")?;
        let cutoff = run + RECOVERY_MAX_SECONDS * 1000;
        let before = cutoff
            .checked_sub(self.epoch() + 1)
            .ok_or("session: stall was admitted after cutoff")?;
        self.advance_live(before).await?;
        self.read_trace()?;
        if self.count("run.ended") != 0 || self.count("delegation.resolved") != 0 {
            return Err("session: stalled delegation ended before the run cutoff".into());
        }
        self.step(1).await?;
        self.wait_until(
            "session cutoff and renewed device readiness",
            |world| world.count("bridge.ready") == 2,
            0,
        )
        .await?;
        let ended = self
            .trace
            .records
            .iter()
            .find(|row| row["event"] == "run.ended")
            .ok_or("session: ready without closing the stalled run")?;
        if ended["reason"] != "session_ceiling" || ended["elapsed_ms"] != cutoff {
            return Err(format!("session: stalled run did not expire at {cutoff} ms: {ended}"));
        }
        // Release the old HTTP response after its call generation is closed.
        // The next wake uses a distinct negotiation on the same device link.
        self.provider()?.control(Control::ReleaseGoodbye).await?;
        self.wait_until(
            "late model response",
            |world| {
                world
                    .provider
                    .as_ref()
                    .is_some_and(|provider| provider.count("models") == 1)
            },
            0,
        )
        .await
    }

    pub(super) async fn wait_for_call(&mut self, count: usize) -> Result<(), String> {
        self.wait_until(
            "connected call",
            |world| world.count("call.live_connected") == count,
            20,
        )
        .await
    }

    fn provider(&self) -> Result<&super::Provider, String> {
        self.provider.as_ref().ok_or_else(|| "session: missing provider".into())
    }

    fn count(&self, event: &str) -> usize {
        self.trace
            .records
            .iter()
            .filter(|row: &&Value| row["event"] == event)
            .count()
    }

    async fn wait_until(&mut self, label: &str, condition: impl Fn(&Self) -> bool, step_ms: u64) -> Result<(), String> {
        let started = tokio::time::Instant::now();
        loop {
            self.read_trace()?;
            if condition(self) {
                return Ok(());
            }
            if started.elapsed() >= std::time::Duration::from_secs(10) {
                return Err(self.stalled(label));
            }
            self.provider
                .as_mut()
                .ok_or("session: missing provider")?
                .tick()
                .await?;
            self.step(step_ms).await?;
        }
    }
}
