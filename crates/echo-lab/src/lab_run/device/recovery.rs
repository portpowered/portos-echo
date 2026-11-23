//! Independent recovery: only the selected process is stopped.
use super::super::{Driver, Step, run_report::Artifacts};
use super::{Configured, observations, runtime};
use serde_json::json;
use std::{path::Path, time::Duration};

pub(super) fn target(bytes: &[u8]) -> Result<u32, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "device: malformed process list")?;
    let mut lines = text.lines();
    if lines.next().map(str::split_whitespace).map(Iterator::collect::<Vec<_>>) != Some(vec!["PID", "PPID", "NAME"]) {
        return Err("device: malformed process header".into());
    }
    let mut targets = Vec::new();
    for line in lines {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 3 {
            return Err("device: malformed process row".into());
        }
        let pid = numeric(fields[0])?;
        let parent = numeric(fields[1])?;
        if fields[2] == "hassmic" && parent != 1 && pid > 1 {
            targets.push(pid);
        }
    }
    if targets.len() != 1 {
        return Err("device: expected one eligible hassmic process".into());
    }
    Ok(targets[0])
}

fn numeric(text: &str) -> Result<u32, String> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err("device: invalid process identity".into());
    }
    text.parse().map_err(|_| "device: invalid process identity".into())
}

impl Configured<'_> {
    #[allow(clippy::disallowed_methods, reason = "Existing host time")]
    fn adb(&self, args: &[String]) -> Result<Vec<u8>, String> {
        super::super::run_command::controlled(
            &self.options.adb,
            args,
            std::time::Instant::now() + Duration::from_secs(20),
            self.options.cancel.as_deref(),
        )
    }

    fn host_pid(&self) -> Result<u32, String> {
        target(&self.adb(&[
            "shell".into(),
            "ps".into(),
            "-A".into(),
            "-o".into(),
            "PID,PPID,NAME".into(),
        ])?)
    }

    pub(super) fn restart(
        &mut self,
        process: &str,
        count: u64,
        say: &Path,
        index: usize,
        artifacts: &mut Artifacts,
    ) -> Result<(), String> {
        if !matches!(process, "bridge" | "hassmic-host") || !(1..=10).contains(&count) {
            return Err("device: unsupported restart".into());
        }
        for iteration in 1..=count {
            self.iteration = iteration;
            self.recover(process, iteration, say, index, artifacts)?;
        }
        self.iteration = 0;
        Ok(())
    }

    #[allow(clippy::disallowed_methods, reason = "Existing host time")]
    fn replace(&mut self, process: &str, host: u32, artifacts: &mut Artifacts) -> Result<u128, String> {
        let began = std::time::Instant::now();
        if process == "bridge" {
            let result = self.executor.block_on(runtime::controlled(
                async {
                    match self.options.bridge.as_deref() {
                        Some(bridge) => self.state.restart_bridge(bridge, artifacts).await,
                        None => self.state.restart_remote(artifacts).await,
                    }
                },
                self.options.cancel.as_deref(),
            ));
            self.state.startup_result(result)?;
        } else if self.options.on_device.is_some() {
            let offset = self.state.arm_relink()?;
            self.adb(&["shell".into(), "kill".into(), host.to_string()])?;
            let result = self.executor.block_on(runtime::controlled(
                self.state.relinked(offset, artifacts),
                self.options.cancel.as_deref(),
            ));
            self.state.startup_result(result)?;
        } else {
            self.state.observe()?;
            let offset = self.state.trace.records.len();
            self.adb(&["shell".into(), "kill".into(), host.to_string()])?;
            self.executor.block_on(runtime::controlled(
                self.state.reconnected(offset),
                self.options.cancel.as_deref(),
            ))?;
        }
        Ok(began.elapsed().as_millis())
    }

    fn recover(
        &mut self,
        process: &str,
        iteration: u64,
        say: &Path,
        index: usize,
        artifacts: &mut Artifacts,
    ) -> Result<(), String> {
        let host = self.host_pid()?;
        let bridge = self.state.bridge_id()?.ok_or("device: missing bridge identity")?;
        let previous = if process == "bridge" { bridge } else { host };
        let target = if process == "bridge" { None } else { Some(host) };
        observations::append(
            artifacts,
            &json!({"event":"device.restart_started","step":index,
            "elapsed_ms":self.state.elapsed()?,"process":process,"iteration":iteration,
            "bridge_generation":self.state.generation,"target_pid":target,"previous_pid":previous}),
        )?;
        let reconnect = self.replace(process, host, artifacts)?;
        let current_host = self.host_pid()?;
        let current_bridge = self.state.bridge_id()?.ok_or("device: missing recovered bridge")?;
        // On the Echo a killed link ends the bridge too, so only the host run keeps its bridge.
        let kept = current_bridge == bridge || self.options.on_device.is_some();
        if (process == "bridge" && (current_host != host || current_bridge == bridge))
            || (process == "hassmic-host" && (current_host == host || !kept))
        {
            return Err("device: restart did not preserve opposite process".into());
        }
        self.execute(&Step::SayFile(say.to_path_buf()), index, None, artifacts)?;
        if process == "bridge" {
            self.state.bridge_restarts += 1;
        } else {
            self.state.host_restarts += 1;
        }
        let wake = self.state.wakes.last().ok_or("device: missing recovery wake")?;
        observations::append(
            artifacts,
            &json!({"event":"device.restart_recovered","step":index,
            "elapsed_ms":self.state.elapsed()?,"process":process,"iteration":iteration,
            "bridge_generation":self.state.generation,"target_pid":target,"previous_pid":previous,
            "pid":if process == "bridge" { current_bridge } else { current_host },
            "reconnect_ms":reconnect,"run_id":wake["run_id"],"mic_frames":wake["mic_frames"]}),
        )
    }
}

impl runtime::Runtime {
    async fn restart_bridge(&mut self, bridge: &Path, artifacts: &mut Artifacts) -> Result<(), String> {
        self.startup = None;
        self.screen.take();
        // Keep the child owned until close finishes, including cancellation.
        self.bridge.as_mut().ok_or("device: missing bridge")?.close().await?;
        self.bridge.take();
        self.collect(artifacts)?;
        std::fs::remove_file(&self.trace_path).map_err(|_| "device: cannot rotate private trace")?;
        self.rotate_trace()?;
        self.startup = Some(super::runtime::Startup::new("bridge_spawn"));
        self.launch(bridge, &artifacts.root).await
    }

    async fn reconnected(&mut self, offset: usize) -> Result<(), String> {
        loop {
            self.observe()?;
            if self.trace.records[offset..]
                .iter()
                .any(|r| r["event"] == "wyoming.connected")
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_hassmic_target_is_unique_numeric_child_only() {
        assert_eq!(
            target(b"PID PPID NAME\n100 1 hassmic\n101 100 hassmic\n102 100 other\n"),
            Ok(101)
        );
        for rows in [
            "",
            "PID PPID NAME\n100 1 hassmic\n",
            "PID PPID NAME\n101 100 hassmic\n102 100 hassmic\n",
            "PID PPID NAME\n101; 100 hassmic\n",
            "PID PPID NAME\n101 100 hassmic extra\n",
            "PID PPID NAME\n4294967296 100 hassmic\n",
            "PPID PID NAME\n101 100 hassmic\n",
        ] {
            assert!(target(rows.as_bytes()).is_err(), "{rows}");
        }
    }
}
