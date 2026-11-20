//! Hold one runner request across the wake's cutoff, then answer a new wake.
use super::super::RECOVERY_MAX_SECONDS;
use super::{Control, Timing, World};
use serde_json::{Value, json};

pub(super) enum MediaBoundary {
    Waiting,
    Playback(u64),
    Crossed,
}

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
        let milliseconds = if self.media_boundary.is_some() {
            self.speak_across_cutoff(cutoff).await?;
            0
        } else {
            let before = cutoff
                .checked_sub(self.epoch() + 1)
                .ok_or("session: stall was admitted after cutoff")?;
            self.advance_live(before).await?;
            self.read_trace()?;
            if self.count("run.ended") != 0 || self.count("delegation.resolved") != 0 {
                return Err("session: stalled delegation ended before the run cutoff".into());
            }
            1
        };
        self.cross_cutoff(cutoff, milliseconds).await?;
        self.read_trace()?;
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

    async fn cross_cutoff(&mut self, cutoff: u64, milliseconds: u64) -> Result<(), String> {
        let bridge = self.bridge.as_mut().ok_or("session: missing bridge")?;
        // The expired voice owner drops its guarded drain receiver. Expect
        // the plain clock receipt and then renewed bridge readiness.
        bridge
            .send(&json!({"action":"advance_clock","milliseconds":milliseconds}))
            .await?;
        let mut advanced = false;
        let mut ready = false;
        while !advanced || !ready {
            let row = bridge.line().await?;
            match row["event"].as_str() {
                Some("bridge.clock_advanced") if row["milliseconds"] == milliseconds && !advanced => advanced = true,
                Some("bridge.ready") if !ready => ready = true,
                event => return Err(format!("session: unexpected cutoff event {event:?}")),
            }
        }
        self.clock["epoch_ms"] = json!(cutoff);
        Ok(())
    }

    async fn seek_before(&mut self, cutoff: u64) -> Result<(), String> {
        let distance = cutoff
            .checked_sub(self.epoch() + 500)
            .ok_or("session: missed media lead-in")?;
        self.advance_live(distance).await
    }

    async fn speak_across_cutoff(&mut self, cutoff: u64) -> Result<(), String> {
        self.seek_before(cutoff).await?;
        self.provider()?
            .control(Control::Events(
                "speech-start",
                vec![json!({
                    "type":"input_transcript.added","item":{"text":"That was my"}
                })],
            ))
            .await?;
        self.satellite
            .as_ref()
            .ok_or("session: missing satellite")?
            .speak(false)
            .await?;
        self.wait_until(
            "speech notification",
            |world| {
                world
                    .provider
                    .as_ref()
                    .is_some_and(|provider| provider.count("events_sent") == 1)
            },
            0,
        )
        .await?;
        self.advance_live(cutoff - self.epoch()).await?;
        self.read_trace()?;
        if self.count("run.ended") != 0 || self.count("delegation.resolved") != 0 {
            return Err("session: run cutoff interrupted active speech".into());
        }
        self.provider()?
            .control(Control::Events(
                "speech-finish",
                vec![json!({
                    "type":"turn.done","turn":{"role":"user","transcript":"That was my full question."}
                })],
            ))
            .await
    }

    pub(super) async fn prepare_playback(&mut self) -> Result<(), String> {
        self.provider()?.control(Control::Delegate(1)).await?;
        self.wait_until(
            "second runner request",
            |world| {
                world
                    .provider
                    .as_ref()
                    .is_some_and(|provider| provider.count("waiting") == 2)
            },
            20,
        )
        .await?;
        let run = self
            .trace
            .records
            .iter()
            .find(|row| row["event"] == "run.started" && row["run_id"] == "run-2")
            .and_then(|row| row["elapsed_ms"].as_u64())
            .ok_or("session: no second run clock")?;
        let cutoff = run + RECOVERY_MAX_SECONDS * 1000;
        self.seek_before(cutoff).await?;
        self.provider()?.control(Control::ReleaseGoodbye).await?;
        self.media_boundary = Some(MediaBoundary::Playback(cutoff));
        Ok(())
    }

    pub(super) fn playback_pending(&self) -> bool {
        matches!(self.media_boundary, Some(MediaBoundary::Playback(_)))
    }

    pub(super) async fn cross_playback(&mut self, timing: &Timing) -> Result<(), String> {
        if let Some(MediaBoundary::Playback(cutoff)) = self.media_boundary
            && timing.played >= 2880
        {
            if timing.replies_played() != 0 || self.epoch() >= cutoff {
                return Err("session: expected open reply PCM before the cutoff".into());
            }
            self.step(cutoff - self.epoch() + 1).await?;
            self.media_boundary = Some(MediaBoundary::Crossed);
        }
        Ok(())
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
