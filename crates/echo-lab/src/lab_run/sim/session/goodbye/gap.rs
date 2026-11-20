//! Challenge settled playback before the provider sends its native hand-off.
use super::super::goodbye_oracle;
use super::{FOLLOWUP, World, json};
use serde_json::Value;

impl World {
    pub(super) async fn challenge_gap(&mut self) -> Result<Value, String> {
        let before = goodbye_oracle::gap_boundary(&self.satellite.observed.borrow(), &self.clock)?;
        let played_at = self.epoch();
        self.satellite.speak(false).await?;
        self.transcript("gap-user", "user", FOLLOWUP, true).await?;
        self.transcript("gap-reply", "assistant", "Unexpected follow-up.", false)
            .await?;
        self.wait(
            "gap user and assistant control sent",
            |world| {
                let provider = world.provider.observed.borrow();
                ["gap-user", "gap-reply"].iter().all(|tag| {
                    provider["events_sent"]
                        .as_array()
                        .is_some_and(|events| events.contains(&json!(tag)))
                })
            },
            20,
        )
        .await?;
        self.step(20).await?;
        let packets = self.provider.probe_audio().await?;
        for _ in 0..3 {
            self.provider.tick().await?;
            self.step(20).await?;
        }
        self.transcript("gap-done", "assistant", "Unexpected follow-up.", true)
            .await?;
        self.wait(
            "gap assistant completion sent",
            |world| {
                world.provider.observed.borrow()["events_sent"]
                    .as_array()
                    .is_some_and(|events| events.contains(&json!("gap-done")))
            },
            20,
        )
        .await?;
        self.step(20).await?;
        let after = goodbye_oracle::gap_boundary(&self.satellite.observed.borrow(), &self.clock)?;
        let capture = after.capture - before.capture;
        if Some(capture) != u64::try_from(crate::lab_run::sim::m3::fixture().len() / 640).ok()
            || packets != 7
            || !before.same_playback(&after)
            || self.count("reply.played") != 1
            || self.count("delegation.created") != 0
            || self.count("item/tool/call") != 0
            || self.count("session.tool_receipt") != 0
            || self.count("wyoming.run_ended") != 0
        {
            return Err(format!(
                "goodbye: incorrect pre-handoff gap, before={before:?} after={after:?}, RTP packets={packets}"
            ));
        }
        Ok(
            json!({"played_observed_ms":played_at,"serviced_through_ms":self.epoch(),
            "capture_chunks":capture,"provider_transcript_updates":3,"provider_rtp_packets":packets,
            "extra_wire_streams":0,"extra_wire_pcm_bytes":0,"extra_bridge_pcm_frames":0,
            "end_receipts":0,"run_ends":0}),
        )
    }
}
