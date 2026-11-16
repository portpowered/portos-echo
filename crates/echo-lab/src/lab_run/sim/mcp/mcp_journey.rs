use serde_json::{Value, json};

use super::mcp_session::Session;

impl Session {
    pub fn expect_screen(&self, expected: &Value) -> Result<(), String> {
        let mut expected = expected.clone();
        let mut bound = false;
        for field in ["verification_uri", "expires_at_unix"] {
            if expected[field].is_object() {
                if expected[field] != json!({"mcp_field":field}) {
                    return Err("M5: invalid screen binding".into());
                }
                let fixture = self.fixture.as_ref().ok_or("M5: missing authorization fixture")?;
                expected[field] = super::mcp_wire::link_code(super::mcp_session::EPOCH, fixture.base())[field].clone();
                bound = true;
            }
        }
        if bound && expected != self.screen {
            return Err("M5: bound screen mismatch".into());
        }
        super::super::observers::screen(
            &serde_json::to_vec(&self.screen).map_err(|_| "M5: screen encoding")?,
            &expected,
        )
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
        self.dispatch("m5-linked").await?;
        self.reach("session.commentary.append").await?;
        let live = self.live.as_mut().ok_or("M5: missing Live fixture")?;
        let text = live.commentary.recv().await.ok_or("M5: missing delivered commentary")?;
        let actual: Value = serde_json::from_str(&text).map_err(|_| "M5: malformed commentary")?;
        if actual != json!({"type":"session.commentary.append","delegation_id":"m5-linked","text":"m5-linked"}) {
            return Err("M5: delivered commentary mismatch".into());
        }
        let requests = live.requests.lock().map_err(|_| "M5: model observation failed")?;
        if requests.len() != 2 {
            return Err("M5: model request count mismatch".into());
        }
        for request in requests.iter() {
            self.observations.model("m5-linked", request).map_err(str::to_owned)?;
        }
        Ok(())
    }

    pub async fn probe(&mut self) -> Result<(), String> {
        self.admit_call()?;
        if !self.replayed || self.measure()?.tool_call != 1 {
            return Err("M5: probe requires a completed tool call and rejected replay".into());
        }
        self.dispatch("m5-replayed").await?;
        self.reach("mcp.run.failed").await?;
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

    async fn dispatch(&self, id: &str) -> Result<(), String> {
        self.live
            .as_ref()
            .ok_or("M5: missing Live fixture")?
            .events
            .send(
                json!({"type":"session.delegation.created","delegation":{"id":id,"target":"client","input":"echo"}})
                    .to_string(),
            )
            .await
            .map_err(|_| "M5: delegation control failed".into())
    }

    async fn reach(&mut self, event: &str) -> Result<(), String> {
        loop {
            self.observe()?;
            if self.trace.records.iter().any(|r| r["event"] == event) {
                return Ok(());
            }
            if self.bridge.as_mut().ok_or("M5: missing bridge")?.exited()? {
                return Err("M5: bridge exited before resource observation".into());
            }
            // Each acknowledged tick drives the paused production owner. Poll
            // real loopback peers between ticks; the outer scenario owns timeout.
            self.advance_bridge(20).await?;
            self.elapsed_ms += 20;
            if self.elapsed_ms.is_multiple_of(1000) {
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
