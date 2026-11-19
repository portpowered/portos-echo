//! A Played barrier permits another turn; only the final oracle waits for idle.
use super::Runtime;
use serde_json::{Value, json};

pub(in crate::lab_run::device) fn selected(options: &crate::lab_run::run_options::Options) -> Result<bool, String> {
    let mut selected = false;
    for path in &options.scenarios {
        let scenario = crate::lab_validate::load(path)?;
        selected |= matches!(
            scenario["id"].as_str(),
            Some("session-two-turns" | "session-idle-timeout")
        );
    }
    if selected && options.scenarios.len() != 1 {
        return Err("device: unmet prerequisite: isolated session scenario".into());
    }
    Ok(selected)
}

impl Runtime {
    pub async fn complete(&mut self, index: usize) -> Result<(), String> {
        let value = self.bridge_event("bridge.run_completed").await?;
        self.completed(&value, index, None)
    }

    fn completed(&mut self, value: &Value, index: usize, expected: Option<&str>) -> Result<(), String> {
        self.observe()?;
        let id = value["run_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("device: missing completed run")?;
        if let Some(expected) = expected {
            if id != expected {
                return Err("device: unmet prerequisite: session completed run identity".into());
            }
        } else if self.run.as_deref() == Some(id) {
            return Err("device: stale completed run".into());
        }
        if self.trace.records[self.wake_offset..]
            .iter()
            .any(|row| row["event"] == "wyoming.run_started" && row["run_id"] != id)
        {
            return Err("device: completed run does not match observed wake".into());
        }
        self.run = Some(id.into());
        self.expect_trace(
            &[
                json!("wyoming.run_started"),
                json!("wyoming.mic_frames {count_gte: 10}"),
                json!("wyoming.run_ended"),
            ],
            index,
        )?;
        let frames = self
            .trace
            .records
            .iter()
            .find(|r| r["event"] == "wyoming.mic_frames" && r["run_id"] == id)
            .and_then(|r| r["count"].as_u64())
            .ok_or("device: missing frame measurement")?;
        self.wakes
            .push(json!({"bridge_generation":self.generation,"run_id":id,"mic_frames":frames}));
        Ok(())
    }

    // Called only AFTER the independent current-wire-run Played barrier. Trace
    // visibility may lag the socket receipt; it is never used to replace Played.
    pub async fn session_reply(&mut self, replies: usize) -> Result<(), String> {
        let selectors = super::selectors::parse(&[
            json!("run.started"),
            json!(format!("reply.played {{outcome: played, count: {replies}}}")),
        ])?;
        loop {
            self.observe()?;
            let records = &self.trace.records[self.wake_offset..];
            let starts: Vec<_> = records.iter().filter(|r| r["event"] == "run.started").collect();
            if starts.len() > 1
                || records
                    .iter()
                    .any(|r| matches!(r["event"].as_str(), Some("wyoming.run_ended" | "bridge.error")))
            {
                return Err("device: session ended or failed before reply barrier".into());
            }
            if let [started] = starts.as_slice() {
                let id = started["run_id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or("device: unmet prerequisite: session run identity")?;
                if self.run.as_deref().is_some_and(|previous| previous != id) {
                    return Err("device: unmet prerequisite: unchanged session run identity".into());
                }
                if super::selectors::check(records, &selectors, Some(id)).is_ok() {
                    self.run = Some(id.into());
                    return Ok(());
                }
            }
            if self.bridge_exited()? {
                return Err("device: unmet prerequisite: session reply before bridge exit".into());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    pub async fn finish_session(&mut self, index: usize) -> Result<(), String> {
        let expected = self
            .run
            .clone()
            .ok_or("device: unmet prerequisite: session reply run identity")?;
        let value = self.bridge_event("bridge.run_completed").await?;
        self.completed(&value, index, Some(&expected))
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
