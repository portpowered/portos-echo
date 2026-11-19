use serde_json::{Value, json};

use super::mcp_session::Session;

impl Session {
    pub fn expect_screen(&self, expected: &Value) -> Result<(), String> {
        let base = self.fixture.as_ref().map(echo_lab::lab_mcp::Fixture::base);
        expect_screen(expected, &self.screen, base)
    }

    pub fn admit_call(&self) -> Result<(), String> {
        if !self.started() || self.pending.is_some() || self.grant()?.expose()["grant"].is_null() {
            return Err("M5: resource control requires a running linked bridge".into());
        }
        Ok(())
    }

    pub async fn call(&mut self) -> Result<(), String> {
        self.admit_call()?;
        if self.trace.records.iter().any(|r| r["event"] == "run.started") {
            return Err("M5: tool journey already started".into());
        }
        let host = self.host.as_mut().ok_or("M5: missing host")?;
        host.send(&json!({"action":"wake"})).await?;
        host.expect("host.ack").await?;
        self.dispatch().await?;
        self.reach(1).await?;
        let state = self
            .live
            .as_ref()
            .ok_or("M5: missing Live fixture")?
            .observed
            .borrow()
            .clone();
        let requests = state["requests"]
            .as_array()
            .ok_or("M5: missing native model observations")?;
        if requests.len() != 2
            || state["delegations"] != json!(["m5-linked"])
            || state["appends"] != json!(["m5-linked"])
        {
            return Err("M5: model request count mismatch".into());
        }
        for request in requests {
            self.observations
                .codex_model("m5-thread", request)
                .map_err(str::to_owned)?;
        }
        Ok(())
    }

    pub async fn probe(&mut self) -> Result<(), String> {
        self.admit_call()?;
        if !self.replayed || self.measure()?.tool_call != 1 {
            return Err("M5: probe requires a completed tool call and rejected replay".into());
        }
        self.dispatch().await?;
        // This is an I/O-only probe on the already established call. Keep its
        // clock fixed while the rejected request and native failure output
        // settle; polling must not consume the control deadline.
        self.reach(2).await?;
        self.native_results()?;
        loop {
            self.receive_screen().await?;
            if self.screen["state"] == "error" {
                break;
            }
        }
        if self.screen
            != json!({"state":"error","relink":true,
            "message":"Run echo-bridge link with your config to relink Port OS MCP."})
        {
            return Err("M5: replay relink screen mismatch".into());
        }
        let resource = self
            .spoof
            .as_ref()
            .ok_or("M5: missing resource")?
            .snapshot()
            .map_err(|_| "M5: resource observation failed")?;
        if resource
            .requests
            .iter()
            .filter(|r| r.authorization_matches == Some(false))
            .count()
            != 1
            || resource.calls.len() != 1
        {
            return Err("M5: replay resource rejection mismatch".into());
        }
        Ok(())
    }

    async fn dispatch(&self) -> Result<(), String> {
        self.live
            .as_ref()
            .ok_or("M5: missing Live fixture")?
            .events
            .send(())
            .await
            .map_err(|_| "M5: delegation control failed".into())
    }

    fn native_results(&mut self) -> Result<(), String> {
        let state = self
            .live
            .as_ref()
            .ok_or("M5: missing provider")?
            .observed
            .borrow()
            .clone();
        let expected = json!(["m5-linked", "m5-replayed"]);
        let requests = state["requests"]
            .as_array()
            .ok_or("M5: missing native model receipts")?;
        if state["delegations"] != expected || state["appends"] != expected || requests.len() != 4 {
            return Err("M5: native delegation/model/append count mismatch".into());
        }
        for request in requests {
            self.observations
                .codex_model("m5-thread", request)
                .map_err(str::to_owned)?;
        }
        self.observe()?;
        let received: Vec<_> = self
            .trace
            .records
            .iter()
            .filter(|r| r["event"] == "item/tool/call")
            .collect();
        let attempts: Vec<_> = self
            .trace
            .records
            .iter()
            .filter(|r| r["event"] == "tools/call")
            .collect();
        if received.len() != 2 || attempts.len() != 2 {
            return Err("M5: dispatch count mismatch".into());
        }
        for (index, (request, attempt)) in received.iter().zip(attempts).enumerate() {
            if request["call_id"] != format!("m5-call-{index}")
                || request["name"] != "test.echo"
                || !["thread_id", "turn_id", "call_id", "run_id", "name"]
                    .into_iter()
                    .all(|key| !request[key].is_null() && request[key] == attempt[key])
            {
                return Err("M5: native dispatch correlation mismatch".into());
            }
        }
        if received[0]["thread_id"] != received[1]["thread_id"] || received[0]["turn_id"] == received[1]["turn_id"] {
            return Err("M5: native turn identity mismatch".into());
        }
        Ok(())
    }

    async fn reach(&mut self, appends: usize) -> Result<(), String> {
        loop {
            self.observe()?;
            let live = self.live.as_ref().ok_or("M5: missing Live fixture")?;
            if live.observed.borrow()["appends"]
                .as_array()
                .is_some_and(|items| items.len() == appends)
            {
                return Ok(());
            }
            if live.observed.has_changed().is_err() {
                return Err("M5: canonical provider ended before append".into());
            }
            if self.bridge.as_mut().ok_or("M5: missing bridge")?.exited()? {
                return Err("M5: bridge exited before resource observation".into());
            }
            // Negotiation needs the media cadence. Once established, the replay
            // probe polls without consuming the turn deadline or grant lifetime.
            self.live.as_mut().ok_or("M5: missing provider")?.tick().await?;
            let milliseconds = if appends == 1 { 20 } else { 0 };
            self.advance_bridge(milliseconds).await?;
            self.elapsed_ms += milliseconds;
            if milliseconds != 0 && self.elapsed_ms.is_multiple_of(1000) {
                self.fixture
                    .as_ref()
                    .ok_or("M5: missing authorization fixture")?
                    .advance(1)
                    .map_err(|_| "M5: fixture clock failed")?;
            }
            tokio::task::yield_now().await;
        }
    }
}

// Bind to the independently known authorization fixture, never to the observed
// screen itself. Every bound state is compared in full before the typed oracle.
fn expect_screen(expected: &Value, screen: &Value, base: Option<&str>) -> Result<(), String> {
    let mut expected = expected.clone();
    let mut bound = false;
    for field in ["user_code", "verification_uri", "expires_at_unix"] {
        if expected[field].is_object() {
            if expected[field] != json!({"mcp_field":field}) {
                return Err("M5: invalid screen binding".into());
            }
            let base = base.ok_or("M5: missing authorization fixture")?;
            expected[field] = super::mcp_wire::link_code(super::mcp_session::EPOCH, base)[field].clone();
            bound = true;
        }
    }
    if bound && expected != *screen {
        return Err("M5: bound screen mismatch".into());
    }
    super::super::observers::screen(
        &serde_json::to_vec(screen).map_err(|_| "M5: screen encoding")?,
        &expected,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_screen_bindings_require_exact_independent_code_uri_expiry_and_full_state() {
        let base = "http://127.0.0.1:18080";
        let screen = super::super::mcp_wire::link_code(super::super::mcp_session::EPOCH, base);
        let expected = json!({"state":"link_code","user_code":{"mcp_field":"user_code"},
            "verification_uri":{"mcp_field":"verification_uri"},
            "expires_at_unix":{"mcp_field":"expires_at_unix"}});
        assert_eq!(screen["user_code"], "TEST-CODE");
        assert_eq!(expect_screen(&expected, &screen, Some(base)), Ok(()));
        for (field, value) in [
            ("user_code", json!("OTHER-CODE")),
            ("verification_uri", json!("http://127.0.0.1:18081/verify")),
            ("expires_at_unix", json!(0)),
            ("state", json!("ready")),
            ("extra", json!(true)),
        ] {
            let mut changed = screen.clone();
            changed[field] = value;
            assert_eq!(
                expect_screen(&expected, &changed, Some(base)),
                Err("M5: bound screen mismatch".into())
            );
        }
        assert_eq!(
            expect_screen(&expected, &screen, None),
            Err("M5: missing authorization fixture".into())
        );
        for binding in [
            json!({"mcp_field":"verification_uri"}),
            json!({"mcp_field":"user_code","extra":true}),
        ] {
            let mut changed = expected.clone();
            changed["user_code"] = binding;
            assert_eq!(
                expect_screen(&changed, &screen, Some(base)),
                Err("M5: invalid screen binding".into())
            );
        }
    }
}
