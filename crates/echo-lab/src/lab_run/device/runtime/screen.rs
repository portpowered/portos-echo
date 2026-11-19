//! Ordered typed screen observations, independent of bridge trace assertions.
use super::super::super::{run_report::Artifacts, sim::observers};
use super::Runtime;
use serde_json::{Value, json};

impl Runtime {
    pub(super) async fn ui_sequence(
        &mut self,
        sequence: &[Value],
        index: usize,
        artifacts: &mut Artifacts,
    ) -> Result<(), String> {
        if sequence.is_empty() {
            return Err("device: empty screen sequence".into());
        }
        let screen = self.screen.as_mut().ok_or("device: screen not connected")?;
        for (position, expected) in sequence.iter().enumerate() {
            let previous = screen.sequence();
            let frame = if position == 0 {
                screen.current()?
            } else {
                screen.next().await?
            };
            if position > 0 && frame["seq"].as_u64() != previous.checked_add(1) {
                return Err("screen: stale or missing presentation in sequence".into());
            }
            let bytes = serde_json::to_vec(&frame["state"]).map_err(|_| "screen: invalid state")?;
            observers::screen(&bytes, expected)?;
            // Publish the matched authored expectation, never arbitrary child text.
            super::super::observations::append(
                artifacts,
                &json!({
                    "event":"device.ui", "step":index, "seq":frame["seq"],
                    "bridge_generation":self.generation, "state":expected
                }),
            )?;
        }
        Ok(())
    }
}
