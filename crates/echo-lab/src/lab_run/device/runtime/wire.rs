//! Run-owned forwarding for measured device journeys. Never retain room PCM.
mod ledger;
mod relay;
#[cfg(test)]
mod tests;

use super::super::super::{run_command, run_options::Options, run_report::Artifacts};
use serde_json::json;
use std::{net::SocketAddr, path::PathBuf, time::Duration};

pub(in crate::lab_run::device) struct Observer {
    adb: PathBuf,
    forward: u16,
    relay: relay::Relay,
}

pub(in crate::lab_run::device) fn endpoint(options: &Options, config: &str) -> Result<Option<SocketAddr>, String> {
    let mut needed = false;
    for path in &options.scenarios {
        let scenario = crate::lab_validate::load(path)?;
        needed |= matches!(
            scenario["id"].as_str(),
            Some(
                "m3-full-duplex"
                    | "m4-spoof-tool"
                    | "session-two-turns"
                    | "session-idle-timeout"
                    | "session-barge-continues"
            )
        );
    }
    if !needed {
        return Ok(None);
    }
    let config = echo_config::BridgeConfig::from_toml_str(config).map_err(|_| "device: invalid wire configuration")?;
    let address: SocketAddr = format!("{}:{}", config.echo.host, config.echo.wyoming_port)
        .parse()
        .map_err(|_| "device: unmet prerequisite: numeric loopback wire endpoint")?;
    if address.ip() != std::net::Ipv4Addr::LOCALHOST || address.port() == 0 {
        return Err("device: unmet prerequisite: numeric loopback wire endpoint".into());
    }
    Ok(Some(address))
}

fn adb(adb: &std::path::Path, args: &[String], cancel: Option<&std::path::Path>) -> Result<Vec<u8>, String> {
    run_command::controlled(adb, args, std::time::Instant::now() + Duration::from_secs(20), cancel)
        .map_err(|_| "device: unmet prerequisite: owned Wyoming forward".into())
}

impl Observer {
    pub(in crate::lab_run::device) fn start(options: &Options, address: SocketAddr) -> Result<Self, String> {
        // Reserve the nominated endpoint before creating a forward. An occupied
        // endpoint is never stolen, and configuration is never rewritten.
        let listener = std::net::TcpListener::bind(address)
            .map_err(|_| "device: unmet prerequisite: nominated wire endpoint unavailable")?;
        listener
            .set_nonblocking(true)
            .map_err(|_| "device: wire listener setup failed")?;
        let output = adb(
            &options.adb,
            &["forward".into(), "tcp:0".into(), "tcp:16700".into()],
            options.cancel.as_deref(),
        )?;
        let text = std::str::from_utf8(&output)
            .map_err(|_| "device: malformed owned forward port")?
            .trim();
        let forward = text
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .filter(|_| text.bytes().all(|byte| byte.is_ascii_digit()))
            .ok_or("device: malformed owned forward port; coordinator recovery required")?;
        Ok(Self {
            adb: options.adb.clone(),
            forward,
            relay: relay::Relay::new(listener),
        })
    }

    pub(in crate::lab_run::device) fn launch(&mut self) -> Result<(), String> {
        self.relay
            .start(SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, self.forward)))
    }

    pub(in crate::lab_run::device) async fn close(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
        let result = self.relay.close().await;
        // Cancellation must not suppress teardown of this run's numeric port.
        let release = adb(
            &self.adb,
            &["forward".into(), "--remove".into(), format!("tcp:{}", self.forward)],
            None,
        );
        let mut failures = Vec::new();
        failures.extend(result.err());
        failures.extend(release.err());
        for receipt in self.relay.receipts() {
            failures.extend(
                super::super::observations::append(
                    artifacts,
                    &json!({
                        "event":"device.wire", "direction":receipt.direction,
                        "kind":receipt.kind,"elapsed_us":receipt.elapsed_us,
                        "pcm_bytes":receipt.pcm_bytes,
                        "sequence":receipt.sequence,"wire_run":receipt.wire_run,
                        "playback_stream":receipt.playback_stream
                    }),
                )
                .err(),
            );
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}
