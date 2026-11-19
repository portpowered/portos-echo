use super::super::run_report::Artifacts;
use super::runtime::Runtime;
use std::io::Write;

impl Runtime {
    pub(super) fn configure_portos(&mut self, config: &str) -> Result<(), String> {
        let config =
            echo_config::BridgeConfig::from_toml_str(config).map_err(|_| "device: invalid Port OS configuration")?;
        let servers = &config.mcp.server;
        if servers.len() != 1 || servers[0].auth != echo_config::McpAuth::PortosDeviceCode {
            return Err("device: M5 requires one Port OS device-code server".into());
        }
        let server = &servers[0];
        self.portos = Some(echo_lab::lab_mcp::PortosObservations::new(
            &server.name,
            &server.allow_tools,
        )?);
        Ok(())
    }

    pub fn observe(&mut self) -> Result<(), String> {
        let result = self.observe_inner();
        self.invalid |= result.is_err();
        result
    }

    fn observe_inner(&mut self) -> Result<(), String> {
        if self.invalid {
            return Err("device: previously invalid observations".into());
        }
        let bytes = match &self.remote {
            Some(remote) => remote.trace()?,
            None => super::super::sim::observers::bytes(&self.trace_path, 1024 * 1024, "trace")?,
        };
        if bytes.len() < self.offset {
            return Err("trace: bridge truncated its trace".into());
        }
        let first = self.trace.records.len();
        self.trace.append(&bytes[self.offset..])?;
        self.offset = bytes.len();
        if let Some(observer) = &mut self.portos {
            let generation = self
                .generation
                .checked_sub(1)
                .ok_or("device: missing trace generation")?;
            for record in &self.trace.records[first..] {
                observer.trace(generation, record)?;
            }
        }
        Ok(())
    }

    /// Only rotate after stopping and collecting the prior process completely.
    pub(super) fn rotate_trace(&mut self) -> Result<(), String> {
        let result = self.trace.finish().and_then(|()| {
            if let Some(observer) = &mut self.portos {
                observer.restarted(self.generation)?;
            }
            Ok(())
        });
        self.invalid |= result.is_err();
        result?;
        if self.invalid {
            return Err("device: previously invalid observations".into());
        }
        self.trace = super::super::sim::trace::Trace::default();
        self.offset = 0;
        self.expectations.clear();
        self.run = None;
        self.generation += 1;
        Ok(())
    }

    pub(super) fn link_counts(&self, artifacts: &mut Artifacts) -> Result<(), String> {
        let Some(observer) = &self.portos else { return Ok(()) };
        artifacts.link_counts = None;
        if self.invalid {
            return Err("device: invalid M5 observations".into());
        }
        artifacts.link_counts = Some(observer.measure()?);
        Ok(())
    }
}

pub fn append(artifacts: &mut Artifacts, value: &serde_json::Value) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(artifacts.root.join("device-steps.jsonl"))
        .map_err(|_| "device: cannot open observations")?;
    writeln!(file, "{value}").map_err(|_| "device: cannot write observations")?;
    if !artifacts.evidence.iter().any(|s| s == "device-steps.jsonl") {
        artifacts.evidence.push("device-steps.jsonl".into());
    }
    Ok(())
}

pub fn mute_window(artifacts: &mut Artifacts, record: &serde_json::Value) -> Result<(), String> {
    if record["event"] != "mic.mute_window" {
        return Ok(());
    }
    append(
        artifacts,
        &serde_json::json!({"event":"device.mute_window","step":null,
        "elapsed_ms":record["elapsed_ms"],"window_id":record["window_id"],
        "received_frames":record["received_frames"],"forwarded_frames":record["forwarded_frames"]}),
    )
}

#[cfg(test)]
mod tests {
    use super::super::runtime::Runtime;
    use serde_json::json;

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
        let mut runtime = Runtime::default();
        runtime.trace_path = path;
        runtime.arm_mute().unwrap();
        let selector = [json!(
            "mic.mute_window {window_id: mute-1, received_frames: 0, forwarded_frames: 0}"
        )];
        assert!(runtime.expect_trace(&selector, 0).unwrap_err().contains("observed 0"));
    }
}
