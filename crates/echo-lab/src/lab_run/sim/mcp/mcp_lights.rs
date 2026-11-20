//! Oracle for the installed bridge's local Port OS control journey.
use super::mcp_session::Session;
use crate::lab_run::{Step, run_oracles};
use echo_lab::lab_mcp::HomeFault;
use serde_json::{Value, json};

const UTTERANCE: &str = "Computer, turn off twin room lights";

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Journey {
    #[default]
    Lifecycle,
    Twin,
    ReadOnly,
}

pub(in crate::lab_run) fn utterance(text: &str) -> Result<Step, String> {
    if text != UTTERANCE {
        return Err("M5: unsupported Twin room utterance".into());
    }
    Ok(Step::SayText(text.into()))
}

impl Session {
    pub(super) async fn lights(&mut self, text: &str) -> Result<Value, String> {
        utterance(text)?;
        self.admit_call()?;
        if self.trace.records.iter().any(|row| row["event"] == "run.started") {
            return Err("M5: tool journey already started".into());
        }
        let fixture = self.fixture.as_ref().ok_or("M5: missing home fixture")?;
        let counts = fixture.counts().map_err(|_| "M5: authorization observation failed")?;
        let grant = self.grant()?;
        let readonly = self.journey == Journey::ReadOnly;
        let expected_scopes = if readonly {
            "endpoint:read"
        } else {
            "endpoint:read endpoint:write message:send group:read group:manage subscription:read subscription:write"
        };
        if grant.expose()["grant"]["scope"] != expected_scopes {
            return Err("M5: committed grant scope mismatch".into());
        }
        self.wake().await?;
        self.wait_for_media().await?;
        self.dispatch().await?;
        // With transport established and no reply yet, native model and
        // resource I/O can settle at the same bridge clock instant.
        self.live
            .as_ref()
            .ok_or("M5: missing Live fixture")?
            .wait_home_append()
            .await?;
        self.wait_for_reply().await?;
        let fixture = self.fixture.as_ref().ok_or("M5: missing home fixture")?;
        if fixture.counts().map_err(|_| "M5: authorization observation failed")? != counts
            || self.grant()?.expose() != grant.expose()
        {
            return Err("M5: control unexpectedly changed the grant".into());
        }
        self.check_lights(readonly, expected_scopes).await
    }

    async fn check_lights(&self, readonly: bool, scopes: &str) -> Result<Value, String> {
        let fixture = self.fixture.as_ref().ok_or("M5: missing home fixture")?;
        let home = fixture.home_snapshot().map_err(|_| "M5: home snapshot failed")?;
        let denials = fixture.scope_denials().map_err(|_| "M5: scope observations failed")?;
        let control = action(readonly);
        if home != expected_home(readonly, self.home_fault, &control)
            || denials
                != if readonly {
                    vec![json!({"name":"control","arguments":control,"scope":"message:send"})]
                } else {
                    vec![]
                }
        {
            return Err("M5: ordered home calls, exact arguments or final device state mismatch".into());
        }
        let rows: Vec<_> = self
            .trace
            .records
            .iter()
            .filter(|row| row["event"] == "mcp.scope_insufficient")
            .collect();
        if rows.len() != usize::from(readonly)
            || rows
                .iter()
                .any(|row| row["tool"] != "control" || row["missing_scope"] != "message:send")
        {
            return Err("M5: scope diagnostic mismatch".into());
        }
        let health = self.health().await?;
        if health["mcp"] != if readonly { "relink_for_scope" } else { "linked" }
            || health["relink_required"] != readonly
        {
            return Err("M5: MCP health projection mismatch".into());
        }
        Ok(
            json!({"event":"portos.home_control","scopes":scopes,"home":home,"denials":denials,
            "health":health["mcp"],"reply":self.transcript()?,"diagnostics":rows}),
        )
    }

    async fn health(&self) -> Result<Value, String> {
        let address = self
            .trace
            .records
            .iter()
            .find(|row| row["event"] == "bridge.started")
            .and_then(|row| row["health_addr"].as_str())
            .ok_or("M5: missing health listener")?;
        let url = format!("http://{address}/healthz");
        let address = super::mcp_wire::endpoint(&url)?;
        if address.ip() != std::net::Ipv4Addr::LOCALHOST || address.port() == 0 {
            return Err("M5: health listener must be loopback".into());
        }
        super::mcp_wire::health(address).await
    }

    fn transcript(&self) -> Result<String, String> {
        let live = self.live.as_ref().ok_or("M5: missing Live fixture")?;
        let value = live.observed.borrow();
        if value["transcripts"][0]["text"] != UTTERANCE {
            return Err("M5: wrong utterance in correlated sideband transcript".into());
        }
        value["transcripts"][1]["text"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "M5: missing assistant reply transcript".into())
    }

    pub(super) fn expect_reply(&self, phrases: &[String]) -> Result<(), String> {
        run_oracles::speech(self.transcript()?.as_bytes(), phrases)
    }

    async fn wait_for_media(&mut self) -> Result<(), String> {
        let mut observed = self.live.as_ref().ok_or("M5: missing Live fixture")?.observed.clone();
        loop {
            let live = self.live.as_mut().ok_or("M5: missing Live fixture")?;
            tokio::select! {
                biased;
                ready = observed.wait_for(|state| state["media_ready"] == true) => {
                    return ready.map(|_| ()).map_err(|_| "M5: provider ended before home media readiness".into());
                }
                tick = live.tick() => tick?,
            }
            self.advance_media().await?;
        }
    }

    async fn wait_for_reply(&mut self) -> Result<(), String> {
        loop {
            self.observe()?;
            let live = self.live.as_mut().ok_or("M5: missing Live fixture")?;
            let played = self
                .trace
                .records
                .iter()
                .filter(|row| row["event"] == "reply.played")
                .count();
            if played > 1 {
                return Err("M5: received multiple home playback receipts".into());
            }
            if played == 1 {
                // The bridge can observe Played before the independent
                // provider stdout reader has received both transcripts.
                return live
                    .observed
                    .wait_for(|state| state["transcripts"].as_array().is_some_and(|items| items.len() == 2))
                    .await
                    .map(|_| ())
                    .map_err(|_| "M5: provider ended before home transcripts".into());
            }
            if live.observed.has_changed().is_err() || self.bridge.as_mut().ok_or("M5: missing bridge")?.exited()? {
                return Err("M5: home reply ended before playback".into());
            }
            live.tick().await?;
            self.advance_bridge(20).await?;
            self.elapsed_ms += 20;
        }
    }
}

fn action(readonly: bool) -> Value {
    json!({"id":if readonly {"light-a"} else {"group-twin-room"},"message":"power/turn-off",
        "payload":{},"idempotencyKey":"twin-room-off-1","waitForResult":!readonly})
}

fn expected_home(readonly: bool, fault: Option<HomeFault>, control: &Value) -> Value {
    let calls = if readonly {
        let query = json!({"name":"endpoint_query","arguments":{"query":{"name":{"eq":"Twin one"}}}});
        let reading = json!({"name":"endpoint_query","arguments":{"query":{"name":{"eq":"Twin one"}},
            "expand":["power"],"forceDeviceAttributeQuery":true}});
        vec![query, reading]
    } else {
        let mut calls = vec![json!({"name":"group_query","arguments":{
            "query":{"name":{"eq":"Twin room"},"role":{"eq":"ROOM"}}
        }})];
        if !matches!(fault, Some(HomeFault::NoRoom | HomeFault::AmbiguousRoom)) {
            calls.push(json!({"name":"control","arguments":control}));
        }
        if fault.is_none() {
            calls.push(
                json!({"name":"endpoint_query","arguments":{"query":{"id":{"in":["light-a","light-b"]}},
                "expand":["power"],"forceDeviceAttributeQuery":true}}),
            );
        }
        calls
    };
    let lights_on = readonly || fault.is_some();
    json!({"calls":calls,"lights_on":[lights_on,lights_on],"plug_on":true})
}
