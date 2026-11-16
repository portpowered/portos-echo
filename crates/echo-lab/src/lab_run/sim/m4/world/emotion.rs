//! M6 extends the same production World and paired screen connection as M4.
use super::{Screen, Value, World};

impl World {
    pub(super) fn configure_emotion(&self) -> Result<(), String> {
        if !self.emotion {
            return Ok(());
        }
        let path = self.root.join("bridge.toml");
        let config = std::fs::read_to_string(&path).map_err(|_| "m6: missing configuration")?;
        std::fs::write(
            path,
            config.replace("allow_tools = [\"test.echo\"]", "allow_tools = [\"ui.show_emotion\"]"),
        )
        .map_err(|_| "m6: cannot select emotion tool".into())
    }

    pub(super) async fn observe(&mut self, screen: &mut Screen, index: usize, name: &str) -> Result<Value, String> {
        if !self.emotion {
            return screen.expect(name).await;
        }
        let frame = screen.next().await?;
        // Retain actual values even if this frame is the failing seed.
        self.ui_states.push(frame.clone());
        let previous = self.ui_states.iter().rev().nth(1).and_then(|v| v["seq"].as_u64());
        super::super::emotion::frame(&frame, previous, &super::super::emotion::sequence()[index])?;
        Ok(frame["state"].clone())
    }

    pub(super) async fn animate(&mut self, screen: &mut Screen) -> Result<(), String> {
        self.observe(screen, 2, "emotion").await?;
        let bridge = self.bridge.as_mut().ok_or("m6: missing bridge")?;
        bridge
            .send(&serde_json::json!({"action":"advance_clock","milliseconds":160}))
            .await?;
        let advanced = bridge.expect("bridge.clock_advanced").await?;
        if advanced["milliseconds"] != 160 {
            return Err("m6: clock acknowledgment mismatch".into());
        }
        self.observe(screen, 3, "listening").await?;
        self.result_gate
            .take()
            .ok_or("m6: missing result gate")?
            .send(())
            .map_err(|()| "m6: result observer abandoned".into())
    }
}
