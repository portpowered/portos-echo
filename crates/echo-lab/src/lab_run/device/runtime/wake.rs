//! Fresh trace evidence stays with the runtime's single trace reader.
use super::Runtime;
use crate::lab_run::{run_device::Wake, run_options::Options};
use std::time::Duration;

pub(in crate::lab_run::device) struct Observer<'a> {
    pub state: &'a mut Runtime,
    pub executor: &'a tokio::runtime::Runtime,
    pub options: &'a Options,
    positive: bool,
    armed: bool,
}

impl<'a> Observer<'a> {
    pub fn new(state: &'a mut Runtime, executor: &'a tokio::runtime::Runtime, options: &'a Options) -> Self {
        Self {
            state,
            executor,
            options,
            positive: false,
            armed: false,
        }
    }
}

impl Wake for Observer<'_> {
    fn arm(&mut self) -> Result<(), String> {
        self.executor.block_on(super::controlled(
            self.state.before_wake(),
            self.options.cancel.as_deref(),
        ))?;
        if self.armed && self.poll()? {
            return Err("device: wake observed before replay; replay refused".into());
        }
        self.state.arm_wake()?;
        self.state.trace.finish()?;
        self.positive = false;
        self.armed = true;
        Ok(())
    }

    fn poll(&mut self) -> Result<bool, String> {
        self.positive |= self.state.fresh_wake()?;
        Ok(self.positive)
    }

    fn wait(&mut self, remaining: Duration) -> Result<bool, String> {
        let deadline = tokio::time::Instant::now() + remaining;
        if self.state.bridge_exited()? {
            return Err("process: exited before wake observation".into());
        }
        self.positive |= self
            .executor
            .block_on(self.state.wake_until(deadline, self.options.cancel.as_deref()))?;
        if !self.positive && self.state.bridge_exited()? {
            return Err("process: exited before wake observation".into());
        }
        Ok(self.positive)
    }
}

impl Runtime {
    async fn wake_until(
        &mut self,
        deadline: tokio::time::Instant,
        cancel: Option<&std::path::Path>,
    ) -> Result<bool, String> {
        loop {
            if crate::lab_run::run_command::cancelled(cancel) {
                return Err("run cancelled".into());
            }
            let woke = self.fresh_wake()?;
            if woke || tokio::time::Instant::now() >= deadline {
                self.trace.finish()?;
                return Ok(woke);
            }
            tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + Duration::from_millis(10))).await;
        }
    }

    pub(in crate::lab_run::device) fn fresh_wake(&mut self) -> Result<bool, String> {
        self.observe()?;
        let mut starts = self.trace.records[self.wake_offset..]
            .iter()
            .filter(|row| row["event"] == "wyoming.run_started");
        let Some(started) = starts.next() else { return Ok(false) };
        if starts.next().is_some() {
            return Err("device: duplicate wake in one attempt".into());
        }
        let id = started["run_id"].as_str().ok_or("device: missing wake run identity")?;
        if self.run.as_deref() == Some(id) {
            return Ok(false);
        }
        if self.trace.records[self.wake_offset..]
            .iter()
            .any(|row| row["event"] == "run.started" && row["run_id"] != id)
        {
            return Err("device: wake does not match bridge run identity".into());
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test(start_paused = true)]
    async fn ring_replay_wake_accepts_late_and_exact_six_second_receipts_without_extra_wait() {
        for delay in [3800, 6000] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("trace");
            std::fs::write(&path, "").unwrap();
            let mut state = Runtime {
                trace_path: path.clone(),
                ..Runtime::default()
            };
            state.arm_wake().unwrap();
            let began = tokio::time::Instant::now();
            let writer = async {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                std::fs::write(
                    &path,
                    format!(
                        "{}\n",
                        json!({"event":"wyoming.run_started","run_id":"new","elapsed_ms":1})
                    ),
                )
                .unwrap();
            };
            let (_, observed) = tokio::join!(biased; writer, state.wake_until(began + Duration::from_secs(6), None));
            assert_eq!(observed, Ok(true));
            assert_eq!(began.elapsed(), Duration::from_millis(delay));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn ring_replay_wake_miss_deadline_and_broken_final_record_are_distinct() {
        for bytes in ["", "{\"event\":"] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("trace");
            std::fs::write(&path, "").unwrap();
            let mut state = Runtime {
                trace_path: path.clone(),
                ..Runtime::default()
            };
            state.arm_wake().unwrap();
            std::fs::write(&path, bytes).unwrap();
            let began = tokio::time::Instant::now();
            let result = state.wake_until(began + Duration::from_secs(6), None).await;
            assert_eq!(
                result,
                if bytes.is_empty() {
                    Ok(false)
                } else {
                    Err("trace: incomplete final JSON line".into())
                }
            );
            assert_eq!(began.elapsed(), Duration::from_secs(6));
        }
    }

    #[test]
    fn ring_replay_wake_is_scoped_to_armed_trace_offset_and_new_run() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("trace");
        let old = format!(
            "{}\n",
            json!({"event":"wyoming.run_started","run_id":"run-old","elapsed_ms":1})
        );
        std::fs::write(&path, &old).unwrap();
        let mut state = Runtime {
            trace_path: path.clone(),
            run: Some("run-old".into()),
            ..Runtime::default()
        };
        state.arm_wake().unwrap();
        assert!(!state.fresh_wake().unwrap());
        std::fs::write(
            &path,
            format!(
                "{old}{}\n",
                json!({"event":"wyoming.run_started","run_id":"run-new","elapsed_ms":2})
            ),
        )
        .unwrap();
        assert!(state.fresh_wake().unwrap());
        assert!(state.fresh_wake().unwrap());
        state.arm_wake().unwrap();
        assert!(!state.fresh_wake().unwrap());
    }

    #[test]
    fn ring_replay_wake_rejects_broken_duplicate_and_mismatched_trace() {
        for rows in [
            "not JSON\n".to_owned(),
            ["run-new", "run-new"]
                .map(|id| {
                    format!(
                        "{}\n",
                        json!({"event":"wyoming.run_started","run_id":id,"elapsed_ms":1})
                    )
                })
                .concat(),
            format!(
                "{}\n{}\n",
                json!({"event":"wyoming.run_started","run_id":"run-new","elapsed_ms":1}),
                json!({"event":"run.started","run_id":"run-other","elapsed_ms":2})
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("trace");
            std::fs::write(&path, "").unwrap();
            let mut state = Runtime {
                trace_path: path.clone(),
                ..Runtime::default()
            };
            state.arm_wake().unwrap();
            std::fs::write(&path, rows).unwrap();
            assert!(state.fresh_wake().is_err());
        }
    }
}
