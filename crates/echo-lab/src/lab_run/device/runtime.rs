use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use echo_live::fake::{FakeLive, FakeResponse, FakeScript};
use serde_json::{Value, json};

use super::super::{
    run_report::Artifacts,
    sim::{process::Process, production::Screen, selectors, trace::Trace},
};

#[derive(Default)]
pub struct Runtime {
    bridge: Option<Process>,
    live: Option<FakeLive>,
    screen: Option<Screen>,
    trace: Trace,
    trace_path: PathBuf,
    offset: usize,
    mute_offset: usize,
    pub run: Option<String>,
    expectations: Vec<(usize, Vec<selectors::Selector>, Option<String>, usize)>,
    pub started: bool,
    began: Option<std::time::Instant>,
}

impl Runtime {
    pub async fn start(&mut self, bridge: &Path, config: &str, root: &Path) -> Result<(), String> {
        self.started = true;
        self.began = Some(std::time::Instant::now());
        self.trace_path = root.join("bridge.private.jsonl");
        super::config::stage(root, config)?;
        self.live = Some(
            FakeLive::bind(FakeScript {
                events: vec![json!({"type":"response.audio.done"}).to_string()],
                response: FakeResponse::Answer,
            })
            .await
            .map_err(|_| "device: fake Live startup failed")?,
        );
        let origin = self.live.as_ref().ok_or("device: missing fake Live")?.base_url();
        let mut command = tokio::process::Command::new(bridge);
        for name in [
            "ECHO_LAB_WYOMING_ADDR",
            "ECHO_LAB_LIVE_URL",
            "ECHO_LAB_TRACE_PATH",
            "ECHO_LAB_SESSION_IDLE_SECONDS",
            "ECHO_LAB_UI_STATE_PATH",
            "ECHO_LAB_MCP_URL",
        ] {
            command.env_remove(name);
        }
        command
            .current_dir(root.join("configured.private"))
            .args([
                "--config",
                "bridge.toml",
                "--health-listen",
                "127.0.0.1:0",
                "--live-loopback",
                origin.trim_end_matches('/'),
                "--model-loopback",
                origin.trim_end_matches('/'),
            ])
            .arg("--trace")
            .arg(&self.trace_path);
        self.bridge = Some(Process::start(&mut command)?);
        self.child()?.expect("bridge.ready").await?;
        self.observe()?;
        if !self.trace.records.iter().any(|r| r["event"] == "wyoming.connected") {
            return Err("device: ready without fresh Wyoming connection".into());
        }
        let address = self
            .trace
            .records
            .iter()
            .find(|r| r["event"] == "screen.listening")
            .and_then(|r| r["address"].as_str())
            .ok_or("device: missing screen endpoint")?
            .parse::<std::net::SocketAddr>()
            .map_err(|_| "device: invalid screen endpoint")?;
        if address.ip() != std::net::Ipv4Addr::LOCALHOST || address.port() == 0 {
            return Err("device: screen endpoint must be loopback".into());
        }
        self.screen = Some(Screen::connect(address, true).await?);
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
        )
    }

    pub fn expect_trace(&mut self, values: &[Value], index: usize) -> Result<(), String> {
        self.observe()?;
        let parsed = selectors::parse(values)?;
        let offset = if values
            .iter()
            .any(|v| v.as_str().is_some_and(|s| s.starts_with("mic.mute_window")))
        {
            self.mute_offset
        } else {
            0
        };
        selectors::check(&self.trace.records[offset..], &parsed, self.run.as_deref())?;
        self.expectations.push((index, parsed, self.run.clone(), offset));
        Ok(())
    }

    pub async fn ui(&mut self, expected: &Value, index: usize, artifacts: &mut Artifacts) -> Result<(), String> {
        let screen = self.screen.as_mut().ok_or("device: screen not connected")?;
        screen.expect(expected).await?;
        // expect uses the shared typed UiState oracle. Project only the matched
        // Ready/Muted values, never credentials or arbitrary provider content.
        let state = match expected["state"].as_str() {
            Some("ready") if expected["message"] == "Echo Live ready" => {
                json!({"state":"ready","message":"Echo Live ready"})
            }
            Some("muted") => json!({"state":"muted"}),
            _ => return Err("device: unsupported UI projection".into()),
        };
        let value = json!({"event":"device.ui","step":index,"bridge_generation":1,
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
            let trace = self.observe().and_then(|()| self.trace.finish());
            for (index, selectors, run, offset) in &self.expectations {
                if let Err(error) = trace
                    .as_ref()
                    .map_err(Clone::clone)
                    .and_then(|()| selectors::check(&self.trace.records[*offset..], selectors, run.as_deref()))
                {
                    artifacts.checks.insert(format!("step_{index}"), false);
                    failures.push(error);
                }
            }
            if trace.is_ok() {
                for record in &self.trace.records {
                    failures.extend(artifacts.observation(record).err());
                    failures.extend(super::observations::mute_window(artifacts, record).err());
                }
                let errors = self
                    .trace
                    .records
                    .iter()
                    .filter(|r| r["event"] == "bridge.error")
                    .count();
                artifacts.checks.insert("bridge_errors_absent".into(), errors == 0);
                if errors != 0 {
                    failures.push("device: bridge errors observed".into());
                }
            }
            failures.extend(trace.err());
        }
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
        result = tokio::time::timeout(Duration::from_secs(20), work) => result.map_err(|_| "device: observation timed out")?,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_mute_measurement_must_be_fresh_not_an_earlier_window() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("trace");
        std::fs::write(
            &path,
            format!(
                "{}\n",
                json!({"event":"mic.mute_window","window_id":"mute-1",
            "received_frames":0,"forwarded_frames":0,"elapsed_ms":1})
            ),
        )
        .unwrap();
        let mut runtime = Runtime {
            trace_path: path,
            ..Runtime::default()
        };
        runtime.arm_mute().unwrap();
        let selector = [json!(
            "mic.mute_window {window_id: mute-1, received_frames: 0, forwarded_frames: 0}"
        )];
        assert!(runtime.expect_trace(&selector, 0).unwrap_err().contains("observed 0"));
    }
}
