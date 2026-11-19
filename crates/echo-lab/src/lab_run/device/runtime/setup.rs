//! The configured journey owns exactly its nominated spoof listener.
use super::{Runtime, json};
use crate::lab_run::{device::observations, run_report::Artifacts};
use echo_spoof_mcp::{Options, Spoof};

impl Runtime {
    pub(super) async fn start_spoof(&mut self) -> Result<(), String> {
        if let Some(port) = self.spoof_port {
            self.phase("spoof_bind")?;
            self.spoof = Some(
                Spoof::start(Options {
                    port,
                    ..Options::default()
                })
                .await
                .map_err(|_| "device: unmet prerequisite: nominated spoof MCP endpoint unavailable")?,
            );
        }
        Ok(())
    }

    pub(super) async fn close_spoof(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
        let Some(spoof) = self.spoof.take() else {
            return Ok(());
        };
        let snapshot = spoof.snapshot();
        // Always join the listener, including snapshot/publication failures.
        let shutdown = spoof
            .shutdown()
            .await
            .map_err(|_| "device: spoof MCP cleanup failed".into());
        let observation = snapshot
            .map_err(|_| "device: spoof MCP observation failed".into())
            .and_then(|snapshot| {
                observations::append(
                    artifacts,
                    &json!({"event":"device.spoof", "step":null,
                        "test_echo_calls":snapshot.calls.iter().filter(|call| call.name == "test.echo").count(),
                        "requests":snapshot.requests.len(), "negative_injected":false}),
                )
            });
        crate::lab_run::combine(observation, shutdown)
    }
}
