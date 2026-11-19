//! Ordered typed screen observations, independent of bridge trace assertions.
use super::super::super::{run_report::Artifacts, sim::observers};
use super::Runtime;
use serde_json::{Value, json};

impl Runtime {
    pub async fn trace_step(&mut self, values: &[Value], index: usize) -> Result<(), String> {
        if !values
            .iter()
            .all(|value| value.as_str().is_some_and(|s| s.starts_with("live.closed")))
        {
            return self.expect_trace(values, index);
        }
        let parsed = super::selectors::parse(values)?;
        // Call idle expiry is asynchronous with run completion. Wait on the
        // actual connection receipt; final collection still checks its count.
        loop {
            self.observe()?;
            if super::selectors::check(&self.trace.records, &parsed, None).is_ok() {
                return self.expect_trace(values, index);
            }
            if self.child()?.exited()? {
                return Err("device: unmet prerequisite: live.closed receipt before bridge exit".into());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

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

#[cfg(test)]
mod tests {
    use super::super::super::super::sim::process::Process;
    use super::*;
    use std::time::Duration;

    // T3 component witness: an inert owned child, private trace and virtual
    // clock. Run completion cannot imply a later call-idle close receipt.
    #[tokio::test(start_paused = true)]
    async fn t3_device_t5_m2_close_waits_and_rechecks_late_duplicates() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("trace");
        std::fs::write(&path, "").unwrap();
        let mut command = tokio::process::Command::new("python3");
        command.args(["-c", "import sys; sys.stdin.read()"]);
        let mut runtime = Runtime {
            bridge: Some(Process::start(&mut command).unwrap()),
            trace_path: path.clone(),
            ..Runtime::default()
        };
        let closed = format!("{}\n", json!({"event":"live.closed","elapsed_ms":4000,"reason":"idle"}));
        let write_path = path.clone();
        let write_closed = closed.clone();
        let writer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            std::fs::write(write_path, write_closed).unwrap();
        });
        let began = tokio::time::Instant::now();
        runtime
            .trace_step(&[json!("live.closed {reason: idle, count: 1}")], 2)
            .await
            .unwrap();
        writer.await.unwrap();
        assert!(began.elapsed() >= Duration::from_millis(25));
        tokio::time::resume();
        runtime.bridge.as_mut().unwrap().close().await.unwrap();
        std::fs::write(path, format!("{closed}{closed}")).unwrap();
        let mut artifacts = Artifacts::create(root.path().join("out")).unwrap();
        let error = runtime.collect(&mut artifacts).unwrap_err();
        assert!(error.contains("expected 1 occurrences, observed 2"), "{error}");
        assert_eq!(artifacts.checks["step_2"], false);
    }
}
