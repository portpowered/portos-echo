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
        if self.armed {
            self.state.rearm_wake(&mut self.positive)?;
        } else {
            self.state.arm_wake()?;
        }
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
    fn rearm_wake(&mut self, positive: &mut bool) -> Result<(), String> {
        // fresh_wake collects exactly one snapshot. Check the old attempt before
        // advancing its offset; another observe here could discard a late wake.
        *positive |= self.fresh_wake()?;
        self.trace.finish()?;
        if *positive {
            return Err("device: wake observed before replay; replay refused".into());
        }
        self.wake_offset = self.trace.records.len();
        Ok(())
    }

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
    use std::{io::Write, path::Path};

    async fn complete_row(path: &Path, at: tokio::time::Instant) {
        tokio::time::sleep_until(at).await;
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
    }

    #[test]
    fn m1_live_trace_partial_preserves_the_arm_and_replay_boundary() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("trace");
        std::fs::write(&path, "").unwrap();
        let mut state = Runtime {
            trace_path: path.clone(),
            ..Runtime::default()
        };
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        let options = crate::lab_run::run_options::parse(&[
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/scenarios/m1-hassmic-bridge.yaml"
            )
            .into(),
            "--driver".into(),
            "sim".into(),
            "--bridge".into(),
            std::env::current_exe().unwrap().to_str().unwrap().into(),
            "--out".into(),
            root.path().join("out").to_str().unwrap().into(),
        ])
        .unwrap();
        let mut observer = Observer::new(&mut state, &executor, &options);
        for (id, expected) in [
            ("before-arm", Ok(())),
            (
                "late-first",
                Err("device: wake observed before replay; replay refused".into()),
            ),
        ] {
            let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            write!(
                file,
                "{}",
                json!({"event":"wyoming.run_started","run_id":id,"elapsed_ms":1})
            )
            .unwrap();
            let path = path.clone();
            executor.spawn(async move {
                complete_row(&path, tokio::time::Instant::now() + Duration::from_millis(20)).await;
            });
            assert_eq!(observer.arm(), expected);
            assert_eq!(observer.state.wake_offset, 1);
            assert_eq!(observer.poll(), Ok(expected.is_err()));
            assert_eq!(observer.state.trace.records.last().unwrap()["run_id"], id);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn m1_live_trace_partial_validates_the_tail_before_early_success() {
        for (event, expected) in [
            ("run.started", Ok(true)),
            (
                "wyoming.run_started",
                Err("device: duplicate wake in one attempt".into()),
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
            std::fs::write(
                &path,
                format!(
                    "{}\n{}",
                    json!({"event":"wyoming.run_started","run_id":"current","elapsed_ms":1}),
                    json!({"event":event,"run_id":"current","elapsed_ms":2})
                ),
            )
            .unwrap();
            let began = tokio::time::Instant::now();
            let delay = Duration::from_millis(20);
            let ((), result) = tokio::join!(biased;
                complete_row(&path, began + delay),
                state.wake_until(began + Duration::from_secs(6), None),
            );
            assert_eq!(result, expected);
            assert_eq!(began.elapsed(), delay);
            assert_eq!(state.trace.records.len(), 2);
        }
    }

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
            let ((), observed) = tokio::join!(biased; writer, state.wake_until(began + Duration::from_secs(6), None));
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
    fn ring_replay_rearming_retains_wake_after_the_previous_miss_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("trace");
        std::fs::write(&path, "").unwrap();
        let mut state = Runtime {
            trace_path: path.clone(),
            ..Runtime::default()
        };
        state.arm_wake().unwrap();
        // The original guard snapshot saw no wake. The bridge writes one before
        // replay rearming collects its snapshot, without a thread or timing race.
        assert!(!state.fresh_wake().unwrap());
        std::fs::write(
            &path,
            format!(
                "{}\n",
                json!({"event":"wyoming.run_started","run_id":"late-first","elapsed_ms":1})
            ),
        )
        .unwrap();
        let offset = state.wake_offset;
        let mut positive = false;
        for _ in 0..2 {
            assert_eq!(
                state.rearm_wake(&mut positive),
                Err("device: wake observed before replay; replay refused".into())
            );
            assert!(positive);
            assert_eq!(state.wake_offset, offset);
            assert!(state.fresh_wake().unwrap());
        }
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
