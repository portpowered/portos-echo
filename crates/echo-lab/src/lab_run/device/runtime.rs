use std::{
    path::{Path, PathBuf},
    time::Duration,
};

mod screen;
mod setup;
mod startup;
pub(super) mod wire;

use echo_live::fake::FakeLive;
use serde_json::{Value, json};

use super::super::{
    run_report::Artifacts,
    sim::{process::Process, production::Screen, selectors, trace::Trace},
};

#[derive(Default)]
pub struct Runtime {
    pub(super) plays: Vec<Value>,
    pub(super) clip: Option<Value>,
    pub(super) bridge: Option<Process>,
    live: Option<FakeLive>,
    pub(super) spoof_port: Option<u16>,
    spoof: Option<echo_spoof_mcp::Spoof>,
    real_config: Option<PathBuf>,
    pub(super) screen: Option<Screen>,
    pub(super) trace: Trace,
    pub(super) trace_path: PathBuf,
    pub(super) offset: usize,
    mute_offset: usize,
    wake_offset: usize,
    pub run: Option<String>,
    pub(super) expectations: Vec<(usize, Vec<selectors::Selector>, Option<String>, usize)>,
    pub started: bool,
    pub(super) generation: u64,
    pub(super) errors: usize,
    pub(super) invalid: bool,
    pub(super) wakes: Vec<Value>,
    pub(super) bridge_restarts: u64,
    pub(super) host_restarts: u64,
    pub(super) ready: u64,
    pub(super) window: Option<Value>,
    pub(super) began: Option<std::time::Instant>,
    pub(super) startup: Option<Startup>,
}

pub(super) struct Startup {
    began: tokio::time::Instant,
    phase_began: tokio::time::Instant,
    deadline: tokio::time::Instant,
    phase: &'static str,
    completed: Vec<String>,
}

impl Startup {
    pub(super) fn new(phase: &'static str) -> Self {
        let began = tokio::time::Instant::now();
        Self {
            began,
            phase_began: began,
            deadline: began + Duration::from_secs(20),
            phase,
            completed: Vec::new(),
        }
    }

    fn check(&self) -> Result<(), String> {
        if tokio::time::Instant::now() >= self.deadline {
            return Err("device: observation timed out".into());
        }
        Ok(())
    }

    fn next(&mut self, phase: &'static str) {
        self.completed
            .push(format!("{}:{}ms", self.phase, self.phase_began.elapsed().as_millis()));
        self.phase = phase;
        self.phase_began = tokio::time::Instant::now();
    }

    fn error(&self, error: String) -> String {
        if error != "device: observation timed out" {
            return error;
        }
        format!(
            "{error}; phase={}; elapsed_ms={}; completed=[{}]",
            self.phase,
            self.began.elapsed().as_millis(),
            self.completed.join(",")
        )
    }
}

impl Runtime {
    pub fn startup_result(&self, result: Result<(), String>) -> Result<(), String> {
        result.map_err(|error| match &self.startup {
            Some(state) => state.error(error),
            None => error,
        })
    }

    fn phase(&mut self, phase: &'static str) -> Result<(), String> {
        if let Some(state) = &mut self.startup {
            state.check()?;
            state.next(phase);
        }
        Ok(())
    }

    fn child(&mut self) -> Result<&mut Process, String> {
        self.bridge.as_mut().ok_or_else(|| "device: bridge not started".into())
    }

    pub fn observe(&mut self) -> Result<(), String> {
        let bytes = super::super::sim::observers::bytes(&self.trace_path, 1024 * 1024, "trace")?;
        if bytes.len() < self.offset {
            return Err("trace: bridge truncated its trace".into());
        }
        self.trace.append(&bytes[self.offset..])?;
        self.offset = bytes.len();
        Ok(())
    }

    pub async fn complete(&mut self, index: usize) -> Result<(), String> {
        let value = self.child()?.expect("bridge.run_completed").await?;
        let id = value["run_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("device: missing completed run")?;
        if self.run.as_deref() == Some(id) {
            return Err("device: stale completed run".into());
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

    pub fn expect_trace(&mut self, values: &[Value], index: usize) -> Result<(), String> {
        self.observe()?;
        let parsed = selectors::parse(values)?;
        let global = values
            .iter()
            .all(|value| value.as_str().is_some_and(|s| s.starts_with("live.closed")));
        let run = if global { None } else { self.run.clone() };
        let offset = if global {
            0
        } else if values
            .iter()
            .any(|v| v.as_str().is_some_and(|s| s.starts_with("mic.mute_window")))
        {
            self.mute_offset
        } else {
            self.wake_offset
        };
        selectors::check(&self.trace.records[offset..], &parsed, run.as_deref())?;
        self.expectations.push((index, parsed, run, offset));
        Ok(())
    }

    pub async fn ui(&mut self, expected: &Value, index: usize, artifacts: &mut Artifacts) -> Result<(), String> {
        if let Some(sequence) = expected["sequence"].as_array() {
            return self.ui_sequence(sequence, index, artifacts).await;
        }
        let screen = self.screen.as_mut().ok_or("device: screen not connected")?;
        screen.expect(expected).await?;
        // expect uses the shared typed UiState oracle. Project only the matched
        // Ready/Muted values, never credentials or arbitrary provider content.
        if self.wakes.is_empty() && expected["state"] == "ready" {
            self.ready += 1;
        }
        let state = match expected["state"].as_str() {
            Some("ready") if expected["message"] == "Echo Live ready" => {
                json!({"state":"ready","message":"Echo Live ready"})
            }
            Some("muted") => json!({"state":"muted"}),
            _ => return Err("device: unsupported UI projection".into()),
        };
        let value = json!({"event":"device.ui","step":index,"bridge_generation":self.generation,
            "seq":screen.sequence(),"elapsed_ms":self.began.ok_or("device: missing start time")?.elapsed().as_millis(),"state":state});
        super::observations::append(artifacts, &value)?;
        Ok(())
    }

    pub fn arm_mute(&mut self) -> Result<(), String> {
        if !self.muted() {
            self.observe()?;
            self.mute_offset = self.trace.records.len();
        }
        Ok(())
    }

    pub fn arm_wake(&mut self) -> Result<(), String> {
        self.observe()?;
        self.wake_offset = self.trace.records.len();
        Ok(())
    }

    pub fn muted(&self) -> bool {
        self.screen.as_ref().is_some_and(Screen::muted)
    }

    pub fn confirmed(&self, index: usize, press: u64, after: u64, artifacts: &mut Artifacts) -> Result<(), String> {
        super::observations::append(
            artifacts,
            &json!({"event":"device.operator_confirmed",
            "step":index,"elapsed_ms":self.began.ok_or("device: missing start time")?.elapsed().as_millis(),
            "press":press,"confirmed_after_ms":after}),
        )
    }

    pub async fn close(&mut self, root: &Path, artifacts: &mut Artifacts) -> Result<(), String> {
        self.screen.take();
        let mut failures = Vec::new();
        if let Some(mut child) = self.bridge.take() {
            failures.extend(child.close().await.err());
            failures.extend(self.collect(artifacts).err());
        }
        failures.extend(self.summary(artifacts).err());
        failures.extend(self.close_spoof(artifacts).await.err());
        if let Some(live) = self.live.take() {
            live.shutdown().await;
        }
        for path in [root.join("configured.private"), self.trace_path.clone()] {
            let result = if path.is_dir() {
                std::fs::remove_dir_all(path)
            } else if path.is_file() {
                std::fs::remove_file(path)
            } else {
                Ok(())
            };
            if result.is_err() {
                failures.push("device: private cleanup failed".into());
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

pub async fn controlled<T>(
    work: impl std::future::Future<Output = Result<T, String>>,
    cancel: Option<&Path>,
) -> Result<T, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let cancellation = async {
        loop {
            if cancel.is_some_and(Path::exists) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::select! {
        biased;
        () = cancellation => Err("run cancelled".into()),
        result = tokio::time::timeout_at(deadline, work) => {
            if cancel.is_some_and(Path::exists) {
                return Err("run cancelled".into());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("device: observation timed out".into());
            }
            result.map_err(|_| "device: observation timed out")?
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn device_startup_timeout_retains_completed_phase_durations() {
        let mut state = Startup::new("config_stage");
        let result = controlled(
            async {
                tokio::time::advance(Duration::from_millis(3)).await;
                state.next("fake_live_bind");
                tokio::time::advance(Duration::from_millis(7)).await;
                state.next("bridge_spawn");
                tokio::time::advance(Duration::from_millis(11)).await;
                state.next("bridge_ready");
                std::future::pending::<Result<(), String>>().await
            },
            None,
        )
        .await;
        assert_eq!(
            state.error(result.unwrap_err()),
            "device: observation timed out; phase=bridge_ready; elapsed_ms=20000; completed=[config_stage:3ms,fake_live_bind:7ms,bridge_spawn:11ms]"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn device_startup_empty_history_and_relaunch_reset_are_exact() {
        let mut runtime = Runtime {
            startup: Some(Startup::new("config_stage")),
            ..Runtime::default()
        };
        tokio::time::advance(Duration::from_millis(4)).await;
        assert_eq!(
            runtime.startup_result(Err("device: observation timed out".into())),
            Err("device: observation timed out; phase=config_stage; elapsed_ms=4; completed=[]".into())
        );
        runtime.phase("fake_live_bind").unwrap();
        runtime.startup = Some(Startup::new("bridge_spawn"));
        tokio::time::advance(Duration::from_millis(2)).await;
        runtime.phase("bridge_ready").unwrap();
        tokio::time::advance(Duration::from_millis(5)).await;
        assert_eq!(
            runtime.startup_result(Err("device: observation timed out".into())),
            Err("device: observation timed out; phase=bridge_ready; elapsed_ms=7; completed=[bridge_spawn:2ms]".into())
        );
        assert_eq!(runtime.startup_result(Ok(())), Ok(()));
        assert_eq!(
            runtime.startup_result(Err("process: exited before observation".into())),
            Err("process: exited before observation".into())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn device_startup_cancel_has_priority_and_never_runs_later_work() {
        let root = tempfile::tempdir().unwrap();
        let cancel = root.path().join("cancel");
        std::fs::write(&cancel, "cancel").unwrap();
        let later = std::cell::Cell::new(0);
        let runtime = Runtime {
            startup: Some(Startup::new("bridge_spawn")),
            ..Runtime::default()
        };
        let result = controlled(
            async {
                later.set(1);
                Ok(())
            },
            Some(&cancel),
        )
        .await;
        assert_eq!(runtime.startup_result(result), Err("run cancelled".into()));
        assert_eq!(later.get(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn device_synchronous_overrun_rejects_phase_advance_and_late_success() {
        let mut runtime = Runtime {
            startup: Some(Startup::new("bridge_spawn")),
            ..Runtime::default()
        };
        let result = controlled(
            async {
                tokio::time::advance(Duration::from_millis(20050)).await;
                runtime.phase("bridge_ready")
            },
            None,
        )
        .await;
        assert_eq!(
            runtime.startup_result(result),
            Err("device: observation timed out; phase=bridge_spawn; elapsed_ms=20050; completed=[]".into())
        );
        assert_eq!(
            runtime.startup.as_ref().unwrap().check(),
            Err("device: observation timed out".into())
        );
        let root = tempfile::tempdir().unwrap();
        let cancel = root.path().join("cancel");
        for cancelled in [false, true] {
            let result = controlled(
                async {
                    tokio::time::advance(Duration::from_millis(20050)).await;
                    if cancelled {
                        std::fs::write(&cancel, "cancel").unwrap();
                    }
                    Ok(())
                },
                Some(&cancel),
            )
            .await;
            let error = if cancelled {
                "run cancelled"
            } else {
                "device: observation timed out"
            };
            assert_eq!(result, Err(error.into()));
        }
    }

    #[test]
    fn device_recovery_wake_cannot_reuse_a_previous_connection_run() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("trace");
        let data = ["wyoming.run_started", "wyoming.mic_frames", "wyoming.run_ended"]
            .into_iter()
            .enumerate()
            .map(|(i, event)| {
                format!(
                    "{}\n",
                    json!({"event":event,"elapsed_ms":i,"run_id":"run-1","count":10})
                )
            })
            .collect::<Vec<_>>()
            .concat();
        std::fs::write(&path, data).unwrap();
        let mut runtime = Runtime {
            trace_path: path,
            run: Some("run-1".into()),
            ..Runtime::default()
        };
        runtime.arm_wake().unwrap();
        assert!(
            runtime
                .expect_trace(&[json!("wyoming.run_started")], 0)
                .unwrap_err()
                .contains("observed 0")
        );
        assert_eq!(runtime.wakes.len(), 0);
    }
}
