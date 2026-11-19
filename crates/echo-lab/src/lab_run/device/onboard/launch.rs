//! The configured journey's process steps, for a bridge the Echo runs.
use super::{POLL, Remote};
use crate::lab_run::{
    device::{
        config, observations,
        runtime::{Runtime, Startup},
    },
    run_report::Artifacts,
    sim::{process::Process, production::Screen},
};
use serde_json::{Value, json};
use std::path::Path;

impl crate::lab_run::device::Configured<'_> {
    pub(in crate::lab_run::device) fn mcp(
        &mut self,
        action: crate::lab_run::McpAction,
        index: usize,
        artifacts: &mut Artifacts,
    ) -> Result<(), String> {
        let result = if matches!(
            action,
            crate::lab_run::McpAction::Call | crate::lab_run::McpAction::Probe
        ) {
            self.mcp_query(action, index, artifacts)
        } else {
            self.executor.block_on(crate::lab_run::device::runtime::observed(
                async {
                    if matches!(action, crate::lab_run::McpAction::Restart) {
                        self.state.mcp_restart(&self.config, artifacts).await
                    } else if matches!(action, crate::lab_run::McpAction::Replay) {
                        self.state.mcp_replay(&self.config, &self.options.python).await
                    } else {
                        self.state.mcp_operator(&action, &self.config).await
                    }
                },
                self.options.cancel.as_deref(),
                self.window,
            ))
        };
        self.state.invalid |= result.is_err();
        if result.is_err() {
            artifacts.link_counts = None;
        }
        result
    }
}

impl Runtime {
    fn remote(&mut self) -> Result<&mut Remote, String> {
        self.remote
            .as_mut()
            .ok_or_else(|| "device: bridge on the Echo not selected".into())
    }

    pub(in crate::lab_run::device) fn stage_remote(&mut self, config: &str, root: &Path) -> Result<(), String> {
        let directory = root.join("configured.private");
        std::fs::create_dir(&directory).map_err(|_| "device: cannot create private configuration")?;
        let local = directory.join("bridge.toml");
        config::write_private(&local, config)?;
        if let Some(port) = self.spoof_port {
            self.remote()?.reverse(port)?;
        }
        self.remote()?.stage(&local)
    }

    pub(in crate::lab_run::device) async fn launch_remote(&mut self) -> Result<(), String> {
        self.remote()?.start().await?;
        self.phase("bridge_ready")?;
        self.bridge_event("bridge.ready").await?;
        // The voice owner traces no Wyoming connection; hassmic's log is the witness.
        self.phase("hassmic_link")?;
        self.remote()?.wait_link().await?;
        self.phase("screen_endpoint")?;
        let address = self.screen_address()?;
        let local = self.remote()?.reach(address)?;
        self.screen = Some(Screen::connect(local, true).await.map_err(|error| {
            format!("device: unmet prerequisite: screen stream of the bridge on the Echo with the lab pairing value ({error})")
        })?);
        self.startup.as_ref().ok_or("device: missing startup")?.check()
    }

    /// The next such record in the trace stands in for the host child's stdout line.
    /// The voice owner announces readiness before every wake, so other records are skipped.
    pub(in crate::lab_run::device) async fn bridge_event(&mut self, event: &str) -> Result<Value, String> {
        if self.remote.is_none() {
            return self.child()?.expect(event).await;
        }
        loop {
            // Sampled first: once the process is gone, the trace read below is final.
            let alive = self.remote()?.pid()?.is_some();
            self.observe()?;
            let cursor = self.remote()?.cursor;
            if let Some(position) = self.trace.records[cursor..]
                .iter()
                .position(|record| record["event"] == event)
            {
                self.remote()?.cursor = cursor + position + 1;
                return Ok(self.trace.records[cursor + position].clone());
            }
            if !alive {
                return Err("process: exited before observation".into());
            }
            tokio::time::sleep(POLL).await;
        }
    }

    pub(in crate::lab_run::device) fn bridge_exited(&mut self) -> Result<bool, String> {
        match &self.remote {
            Some(remote) => Ok(remote.pid()?.is_none()),
            None => self.child()?.exited(),
        }
    }

    pub(in crate::lab_run::device) fn bridge_id(&self) -> Result<Option<u32>, String> {
        match &self.remote {
            Some(remote) => remote.pid(),
            None => Ok(self.bridge.as_ref().and_then(Process::id)),
        }
    }

    pub(in crate::lab_run::device) async fn restart_remote(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
        self.startup = None;
        self.screen.take();
        self.remote()?.stop().await?;
        self.collect(artifacts)?;
        self.rotate_trace()?;
        self.startup = Some(Startup::new("bridge_spawn"));
        self.launch_remote().await
    }

    /// Retain the sole grant owner's exact persisted record across a real stop
    /// and launch. Reload counts still require later successful runtime use.
    pub(in crate::lab_run::device) async fn mcp_restart(
        &mut self,
        config: &str,
        artifacts: &mut Artifacts,
    ) -> Result<(), String> {
        if self.portos.is_none() {
            return Err("device: M5 Port OS observer not configured".into());
        }
        if self.pending_mcp.is_some() {
            return Err("device: MCP link already pending".into());
        }
        let stored = self.remote()?.mcp_grant(config)?;
        let result = async {
            self.restart_remote(artifacts).await?;
            if self.remote()?.mcp_grant(config)?.expose() != stored.expose() {
                return Err("device: MCP restart changed the stored grant".into());
            }
            Ok(())
        }
        .await;
        self.invalid |= result.is_err();
        if result.is_err() {
            artifacts.link_counts = None;
        }
        result
    }

    /// Call before killing hassmic: only later trace and log lines belong to the recovery.
    pub(in crate::lab_run::device) fn arm_relink(&mut self) -> Result<usize, String> {
        self.observe()?;
        self.remote()?.mark_link()?;
        Ok(self.trace.records.len())
    }

    /// On the Echo a lost link ends the bridge and its launcher starts it again
    /// (launch contract, point 3). That exit is the kill's consequence, not a bridge fault.
    pub(in crate::lab_run::device) async fn relinked(
        &mut self,
        offset: usize,
        artifacts: &mut Artifacts,
    ) -> Result<(), String> {
        loop {
            if self.bridge_id()?.is_none() {
                self.observe()?;
                self.excused = self.trace.records[offset..]
                    .iter()
                    .filter(|record| record["event"] == "bridge.error")
                    .count();
                observations::append(
                    artifacts,
                    &json!({"event":"device.bridge_link_loss_exit","step":null,
                        "bridge_generation":self.generation,"excused_errors":self.excused}),
                )?;
                return self.restart_remote(artifacts).await;
            }
            if self.remote()?.linked()? {
                return Ok(());
            }
            tokio::time::sleep(POLL).await;
        }
    }

    pub(in crate::lab_run::device) async fn close_remote(&mut self, artifacts: &mut Artifacts) -> Vec<String> {
        let mut failures = Vec::new();
        if !self.started {
            return failures;
        }
        if let Some(remote) = self.remote.as_mut() {
            failures.extend(remote.stop().await.err());
            failures.extend(self.collect(artifacts).err());
        }
        if let Some(remote) = self.remote.as_mut() {
            failures.extend(remote.release().err());
        }
        failures
    }
}
