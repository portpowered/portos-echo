//! Ordered typed screen observations, independent of bridge trace assertions.
use super::super::super::{run_report::Artifacts, sim::observers};
use super::Runtime;
use serde_json::{Value, json};

const MCP_SCREEN: &str = "device: invalid M5 screen observation";
const RELINK: &str = "Run echo-bridge link with your config to relink Port OS MCP.";

// Bind only the pending operation's presentation, in memory. Full equality
// remains required after binding; neither codes nor URIs enter run artifacts.
fn mcp_state(expected: &Value, actual: &Value, now: u64) -> Result<Value, String> {
    let projection = match expected["state"].as_str() {
        Some("link_code") => {
            let code = actual["user_code"].as_str().ok_or(MCP_SCREEN)?;
            let uri = actual["verification_uri"].as_str().ok_or(MCP_SCREEN)?;
            let parsed = url::Url::parse(uri).map_err(|_| MCP_SCREEN)?;
            let local = parsed.host_str().is_some_and(|host| host == "127.0.0.1");
            if code.trim().is_empty()
                || code.len() > 128
                || code.chars().any(char::is_control)
                || uri.len() > 2048
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.host_str().is_none()
                || !(parsed.scheme() == "https" || parsed.scheme() == "http" && local)
                || actual["expires_at_unix"].as_u64().is_none_or(|expiry| expiry <= now)
            {
                return Err(MCP_SCREEN.into());
            }
            json!({"state":"link_code"})
        }
        Some("ready") => json!({"state":"ready","message":"Echo Live ready"}),
        Some("error") => json!({"state":"error","relink":true,"message":RELINK}),
        _ => return Err(MCP_SCREEN.into()),
    };
    let mut bound = expected.clone();
    if expected["state"] == "link_code" {
        for field in ["user_code", "verification_uri", "expires_at_unix"] {
            if expected[field].is_object() {
                if expected[field] != json!({"mcp_field":field}) {
                    return Err(MCP_SCREEN.into());
                }
                bound[field] = actual[field].clone();
            }
        }
    } else if expected != &projection {
        return Err(MCP_SCREEN.into());
    }
    if &bound != actual {
        return Err(MCP_SCREEN.into());
    }
    let bytes = serde_json::to_vec(actual).map_err(|_| MCP_SCREEN)?;
    observers::screen(&bytes, &bound).map_err(|_| MCP_SCREEN)?;
    Ok(projection)
}

impl Runtime {
    #[allow(clippy::disallowed_methods, reason = "Existing host time")]
    pub(super) async fn ui_portos(
        &mut self,
        expected: &Value,
        index: usize,
        artifacts: &mut Artifacts,
    ) -> Result<(), String> {
        let after = if expected["state"] == "link_code" {
            Some(
                self.pending_mcp
                    .as_ref()
                    .and_then(|pending| pending.screen_after)
                    .ok_or(MCP_SCREEN)?,
            )
        } else {
            None
        };
        let screen = self.screen.as_mut().ok_or("device: screen not connected")?;
        loop {
            let frame = screen.current()?;
            if frame["state"]["state"] == expected["state"] {
                if after.is_some_and(|(generation, after)| generation != self.generation || screen.sequence() <= after)
                {
                    return Err(MCP_SCREEN.into());
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| MCP_SCREEN)?
                    .as_secs();
                let projection = mcp_state(expected, &frame["state"], now)?;
                let typed = serde_json::from_value(frame["state"].clone()).map_err(|_| MCP_SCREEN)?;
                self.portos.as_mut().ok_or(MCP_SCREEN)?.screen(&typed);
                return super::super::observations::append(
                    artifacts,
                    &json!({
                        "event":"device.ui", "step":index, "seq":screen.sequence(),
                        "bridge_generation":self.generation, "state":projection
                    }),
                );
            }
            let previous = screen.sequence();
            let frame = screen.next().await?;
            if frame["seq"].as_u64() != previous.checked_add(1) {
                return Err(MCP_SCREEN.into());
            }
        }
    }

    pub async fn trace_step(&mut self, values: &[Value], index: usize) -> Result<(), String> {
        if !values
            .iter()
            .all(|value| value.as_str().is_some_and(|s| s.starts_with("live.closed")))
        {
            return self.expect_trace(values, index);
        }
        let parsed = super::selectors::parse(values)?;
        // Call idle expiry is asynchronous with run completion. Wait on the
        // actual connection receipt; final collection still checks its count.
        loop {
            self.observe()?;
            if super::selectors::check(&self.trace.records, &parsed, None).is_ok() {
                return self.expect_trace(values, index);
            }
            if self.bridge_exited()? {
                return Err("device: unmet prerequisite: live.closed receipt before bridge exit".into());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    pub(super) async fn ui_sequence(
        &mut self,
        sequence: &[Value],
        index: usize,
        artifacts: &mut Artifacts,
    ) -> Result<(), String> {
        if sequence.is_empty() {
            return Err("device: empty screen sequence".into());
        }
        let screen = self.screen.as_mut().ok_or("device: screen not connected")?;
        for (position, expected) in sequence.iter().enumerate() {
            let previous = screen.sequence();
            let frame = if position == 0 {
                screen.current()?
            } else {
                screen.next().await?
            };
            if position > 0 && frame["seq"].as_u64() != previous.checked_add(1) {
                return Err("screen: stale or missing presentation in sequence".into());
            }
            let bytes = serde_json::to_vec(&frame["state"]).map_err(|_| "screen: invalid state")?;
            observers::screen(&bytes, expected)?;
            // Publish the matched authored expectation, never arbitrary child text.
            super::super::observations::append(
                artifacts,
                &json!({
                    "event":"device.ui", "step":index, "seq":frame["seq"],
                    "bridge_generation":self.generation, "state":expected
                }),
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::super::sim::process::Process;
    use super::*;
    use std::time::Duration;

    fn bindings() -> Value {
        json!({"state":"link_code","user_code":{"mcp_field":"user_code"},
            "verification_uri":{"mcp_field":"verification_uri"},
            "expires_at_unix":{"mcp_field":"expires_at_unix"}})
    }

    fn link() -> Value {
        json!({"state":"link_code","user_code":"SYNTHETIC-CODE",
            "verification_uri":"https://issuer.invalid/activate","expires_at_unix":101}) // lint-rules: allow-literal inert presentation, never connected
    }

    #[test]
    fn mcp_device_screen_bindings_require_full_state_and_future_expiry() {
        assert_eq!(mcp_state(&bindings(), &link(), 100), Ok(json!({"state":"link_code"})));
        for field in ["user_code", "verification_uri", "expires_at_unix"] {
            let mut missing = bindings();
            missing.as_object_mut().unwrap().remove(field);
            assert_eq!(mcp_state(&missing, &link(), 100), Err(MCP_SCREEN.into()));
            for invalid in [
                json!({"mcp_field":"other"}),
                json!({"mcp_field":field,"extra":1}),
                json!("*"),
            ] {
                let mut bad = bindings();
                bad[field] = invalid;
                assert_eq!(mcp_state(&bad, &link(), 100), Err(MCP_SCREEN.into()));
            }
        }
        let mut extra = link();
        extra["extra"] = json!(1);
        assert_eq!(mcp_state(&bindings(), &extra, 100), Err(MCP_SCREEN.into()));
        for expiry in [json!(0), json!(100), json!(-1), json!("101"), Value::Null] {
            let mut bad = link();
            bad["expires_at_unix"] = expiry;
            assert_eq!(mcp_state(&bindings(), &bad, 100), Err(MCP_SCREEN.into()));
        }
    }

    #[test]
    fn mcp_device_screen_rejects_bad_codes_uris_and_unrelated_relink_messages() {
        for code in [String::new(), " ".into(), "private\ncode".into(), "x".repeat(129)] {
            let mut bad = link();
            bad["user_code"] = json!(code);
            assert_eq!(mcp_state(&bindings(), &bad, 100), Err(MCP_SCREEN.into()));
        }
        for uri in [
            "not-a-uri",
            "file:///private",
            "http://issuer.invalid/activate", // lint-rules: allow-literal refused URI, never connected
            "https://user@issuer.invalid",    // lint-rules: allow-literal refused URI, never connected
        ] {
            let mut bad = link();
            bad["verification_uri"] = json!(uri);
            assert_eq!(mcp_state(&bindings(), &bad, 100), Err(MCP_SCREEN.into()));
        }
        let ready = json!({"state":"ready","message":"Echo Live ready"});
        let relink = json!({"state":"error","relink":true,"message":RELINK});
        assert_eq!(mcp_state(&ready, &ready, 100), Ok(ready.clone()));
        assert_eq!(mcp_state(&relink, &relink, 100), Ok(relink.clone()));
        for bad in [
            json!({"state":"error"}),
            json!({"state":"error","relink":true,"message":"private"}),
        ] {
            assert_eq!(mcp_state(&bad, &bad, 100), Err(MCP_SCREEN.into()));
            assert_eq!(mcp_state(&relink, &bad, 100), Err(MCP_SCREEN.into()));
        }
    }

    // T3 component witness: an inert owned child, private trace and virtual
    // clock. Run completion cannot imply a later call-idle close receipt.
    #[tokio::test(start_paused = true)]
    async fn t3_device_t5_m2_close_waits_and_rechecks_late_duplicates() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("trace");
        std::fs::write(&path, "").unwrap();
        let mut command = tokio::process::Command::new("python3");
        command.args(["-c", "import sys; sys.stdin.read()"]);
        let mut runtime = Runtime {
            bridge: Some(Process::start(&mut command).unwrap()),
            trace_path: path.clone(),
            ..Runtime::default()
        };
        let closed = format!("{}\n", json!({"event":"live.closed","elapsed_ms":4000,"reason":"idle"}));
        let write_path = path.clone();
        let write_closed = closed.clone();
        let writer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            std::fs::write(write_path, write_closed).unwrap();
        });
        let began = tokio::time::Instant::now();
        runtime
            .trace_step(&[json!("live.closed {reason: idle, count: 1}")], 2)
            .await
            .unwrap();
        writer.await.unwrap();
        assert!(began.elapsed() >= Duration::from_millis(25));
        tokio::time::resume();
        runtime.bridge.as_mut().unwrap().close().await.unwrap();
        std::fs::write(path, format!("{closed}{closed}")).unwrap();
        let mut artifacts = Artifacts::create(root.path().join("out")).unwrap();
        let error = runtime.collect(&mut artifacts).unwrap_err();
        assert!(error.contains("expected 1 occurrences, observed 2"), "{error}");
        assert!(!artifacts.checks["step_2"]);
    }
}
