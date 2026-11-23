//! Legacy dispatch receipts plus bounded native completion observation.
use super::Trace;
use crate::Error;
use echo_codex::codex_protocol::{RequestId, ToolCall, ToolResult, TurnInterrupt};
use echo_delegation::codex_dispatch::Catalog;
use serde_json::json;
use std::sync::Mutex;

/// One configured Port OS host per process. The lock orders ordinals with the
/// unchanged attempt records, even when admitted calls complete out of order.
pub(crate) struct PortosReceipts {
    trace: Trace,
    server: String,
    attempts: Mutex<u64>,
}

impl PortosReceipts {
    pub fn new(trace: Trace, server: String) -> Self {
        Self {
            trace,
            server,
            attempts: Mutex::new(0),
        }
    }

    pub fn catalog(&self, count: usize) -> Result<(), Error> {
        self.trace.clone().record(
            "mcp.tools_list.completed",
            None,
            json!({"server":self.server,"outcome":"success","count":count}),
        )
    }

    pub fn attempt(&self, name: &str, key: &str) -> Result<u64, Error> {
        let mut attempts = self.attempts.lock().map_err(|_| Error::Trace)?;
        let ordinal = attempts.checked_add(1).ok_or(Error::Trace)?;
        self.trace.clone().tool_attempt(name, key)?;
        *attempts = ordinal;
        Ok(ordinal)
    }

    // Only the outcome crosses this boundary; no arguments, result or key.
    pub fn complete(&self, name: &str, ordinal: u64, success: bool) -> Result<(), Error> {
        self.trace.clone().record(
            "mcp.tool.completed",
            None,
            json!({"server":self.server,"name":name,
                "outcome":if success { "success" } else { "error" },"attempt_count":ordinal}),
        )
    }
}

pub(crate) struct ToolTrace {
    trace: Trace,
    names: Vec<(String, String)>,
    native: super::native_delegation::Native,
    pending: std::collections::BTreeMap<RequestId, serde_json::Value>,
}

impl ToolTrace {
    pub fn server(&mut self, server: Option<&str>) {
        self.native.server = server.map(str::to_owned);
    }

    pub fn new(trace: Trace, catalog: &Catalog) -> Self {
        Self {
            native: super::native_delegation::Native::new(trace.clone()),
            pending: std::collections::BTreeMap::new(),
            trace,
            names: catalog
                .tools()
                .iter()
                .filter_map(|tool| {
                    catalog
                        .resolve(&tool.name)
                        .map(|name| (tool.name.clone(), name.to_owned()))
                })
                .collect(),
        }
    }

    pub fn notification(&mut self, method: &str, params: &serde_json::Value) -> Result<(), Error> {
        let pending = self.pending.iter().find(|(_, request)| {
            request["threadId"] == params["threadId"]
                && request["turnId"] == params["turnId"]
                && request["item"]["id"] == params["item"]["id"]
        });
        let mut observed = params.clone();
        if let Some((_, request)) = pending
            && observed["item"].get("arguments").is_none()
        {
            observed["item"]["arguments"] = request["item"]["arguments"].clone();
        }
        self.native
            .tool(method, &observed, &self.names, pending.map(|(id, _)| id))?;
        self.native.notification(method, params)
    }

    pub fn close(&mut self, outcome: &str, reason: Option<&str>) -> Result<(), Error> {
        self.native.close(outcome, reason)
    }

    pub fn timeout(&mut self, turn: &str) {
        self.native.timeout(turn);
    }

    pub fn interrupt(&mut self, event: &str, turn: &TurnInterrupt) -> Result<(), Error> {
        if !valid(&turn.thread_id) || !valid(&turn.turn_id) {
            return Err(Error::Decode);
        }
        let run = self.trace.current_run()?;
        self.trace.record(
            event,
            run.as_deref(),
            json!({"thread_id":turn.thread_id,"turn_id":turn.turn_id}),
        )
    }

    pub fn request(&mut self, id: &RequestId, call: &ToolCall) -> Result<(), Error> {
        let Some((_, name)) = self.names.iter().find(|(alias, _)| alias == &call.tool) else {
            // Unknown model-supplied names are never diagnostic vocabulary.
            return Ok(());
        };
        if ![&call.thread_id, &call.turn_id, &call.call_id]
            .into_iter()
            .all(|id| valid(id))
            || matches!(id, RequestId::Text(text) if !valid(text))
        {
            return Err(Error::Decode);
        }
        let run = self.trace.current_run()?;
        self.trace.record(
            "item/tool/call",
            run.as_deref(),
            json!({
                "name":name,"thread_id":call.thread_id,"turn_id":call.turn_id,"call_id":call.call_id,"request_id":id
            }),
        )?;
        let mut payload = serde_json::Map::new();
        payload.insert("request".into(), call.arguments.clone());
        super::delegation_payload::prepare(&mut payload, self.trace.payloads)?;
        let params = json!({"threadId":call.thread_id,"turnId":call.turn_id,"item":{
            "type":"dynamicToolCall","id":call.call_id,"tool":call.tool,"arguments":payload.remove("request")}});
        self.native.tool("item/started", &params, &self.names, None)?;
        if self.pending.len() >= 1024 {
            return Err(Error::Trace);
        }
        self.pending.entry(id.clone()).or_insert(params);
        Ok(())
    }

    pub fn answer(&mut self, id: &RequestId, result: &ToolResult) -> Result<(), Error> {
        let Some(mut params) = self.pending.remove(id) else {
            return Ok(());
        };
        let declined = !result.success && result.content_items.iter().any(|content| {
            matches!(content, echo_codex::codex_protocol::ToolContent::InputText { text }
                if matches!(text.as_str(), "tool request denied" | "tool request refused" | "tool authorization required"))
        });
        params["item"]["status"] = if result.success {
            "completed"
        } else if declined {
            "declined"
        } else {
            "failed"
        }
        .into();
        params["item"]["success"] = result.success.into();
        params["item"]["contentItems"] = serde_json::to_value(&result.content_items).map_err(|_| Error::Trace)?;
        self.native.tool("item/completed", &params, &self.names, Some(id))
    }
}

impl Trace {
    /// Local completion summary: callers supply only an authored tool/error
    /// and a validated path. Contents and rejected arguments never reach here.
    pub(crate) fn agent_tool(&mut self, tool: &str, path: Option<&str>, bytes: u64, result: &str) -> Result<(), Error> {
        let run = self.current_run()?;
        self.record(
            "agent.tool",
            run.as_deref(),
            json!({"tool":tool,"path":path,"bytes":bytes,"result":result}),
        )
    }

    /// Only the validated prompt selector enters this receipt, never its text.
    pub(crate) fn agent_prompt(
        &mut self,
        tool: &str,
        which: Option<&str>,
        bytes: u64,
        result: &str,
    ) -> Result<(), Error> {
        let run = self.current_run()?;
        self.record(
            "agent.tool",
            run.as_deref(),
            json!({"tool":tool,"which":which,"bytes":bytes,"result":result}),
        )
    }

    /// Record the host attempt after bridge policy/consent, before MCP execution.
    /// The length-prefixed dispatcher key supplies exact correlation without args.
    pub(crate) fn tool_attempt(&mut self, name: &str, key: &str) -> Result<(), Error> {
        let mut remaining = key;
        let thread = take_id(&mut remaining)?;
        let turn = take_id(&mut remaining)?;
        let call = take_id(&mut remaining)?;
        if !remaining.is_empty() || !valid(name) {
            return Err(Error::Trace);
        }
        let run = self.current_run()?;
        self.record(
            "tools/call",
            run.as_deref(),
            json!({
                "name":name,"thread_id":thread,"turn_id":turn,"call_id":call
            }),
        )
    }
}

fn take_id<'a>(remaining: &mut &'a str) -> Result<&'a str, Error> {
    let (prefix, text) = remaining.split_once(':').ok_or(Error::Trace)?;
    let length: usize = prefix.parse().map_err(|_| Error::Trace)?;
    if length.to_string() != prefix {
        return Err(Error::Trace);
    }
    let id = text.get(..length).filter(|id| valid(id)).ok_or(Error::Trace)?;
    *remaining = text.get(length..).ok_or(Error::Trace)?;
    Ok(id)
}

fn valid(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_api::ToolSpec;

    fn catalog() -> Catalog {
        Catalog::new(
            vec![ToolSpec {
                name: "test.echo".into(),
                description: "Echo".into(),
                input_schema: json!({"type":"object"}),
            }],
            &["test.echo".into()],
        )
        .unwrap()
    }

    #[test]
    fn receipts_keep_exact_correlations_and_shared_run_without_payloads() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("trace");
        let trace = Trace::open(&path, "api_key").unwrap();
        let mut request = ToolTrace::new(trace.clone(), &catalog());
        let mut effect = trace.clone();
        let call: ToolCall = serde_json::from_value(json!({"threadId":"t:é", "turnId":"turn", "callId":"call",
            "tool":"test_echo", "arguments":{"text":"private argument"}}))
        .unwrap();
        trace.tool_run(Some("run-1")).unwrap();
        request.request(&RequestId::Number(7), &call).unwrap();
        effect.tool_attempt("test.echo", "4:t:é4:turn4:call").unwrap();
        trace.tool_run(None).unwrap();
        effect.tool_attempt("test.echo", "4:t:é4:turn4:next").unwrap();
        let raw = std::fs::read_to_string(path).unwrap();
        assert!(!raw.contains("private argument"));
        let mut rows: Vec<serde_json::Value> = raw.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        for row in &mut rows {
            row["elapsed_ms"] = json!(0);
        }
        let expected = json!({"event":"tools/call","name":"test.echo","thread_id":"t:é","turn_id":"turn",
            "call_id":"call","run_id":"run-1","elapsed_ms":0,"provider_auth_mode":"api_key"});
        let mut requested = expected.clone();
        requested["event"] = json!("item/tool/call");
        requested["request_id"] = json!(7);
        let mut idle = expected.clone();
        idle.as_object_mut().unwrap().remove("run_id");
        idle["call_id"] = json!("next");
        assert_eq!(rows, vec![requested, expected, idle]);
    }

    #[test]
    fn interrupt_receipts_reject_unsafe_ids_and_record_only_matching_correlation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("trace");
        let trace = Trace::open(&path, "api_key").unwrap();
        trace.tool_run(Some("run-1")).unwrap();
        let mut receipts = ToolTrace::new(trace, &catalog());
        let mut turn = TurnInterrupt {
            thread_id: "thread".into(),
            turn_id: "turn".into(),
        };
        receipts.interrupt("turn/interrupt", &turn).unwrap();
        turn.turn_id = "private\nvalue".into();
        assert_eq!(receipts.interrupt("turn/interrupt", &turn), Err(Error::Decode));
        let mut row: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        row["elapsed_ms"] = json!(0);
        assert_eq!(
            row,
            json!({"event":"turn/interrupt","thread_id":"thread","turn_id":"turn",
            "run_id":"run-1","elapsed_ms":0,"provider_auth_mode":"api_key"})
        );
    }

    #[test]
    fn uncatalogued_alias_produces_no_receipt() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("trace");
        let mut receipts = ToolTrace::new(Trace::open(&path, "api_key").unwrap(), &catalog());
        let call: ToolCall = serde_json::from_value(json!({
            "threadId":"t","turnId":"u","callId":"c","tool":"test_wait","arguments":{}
        }))
        .unwrap();
        receipts.request(&RequestId::Number(1), &call).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"");
    }

    #[test]
    fn invalid_keys_and_unknown_names_cannot_leak_into_receipts() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("trace");
        let mut trace = Trace::open(&path, "api_key").unwrap();
        for key in [
            "",
            "01:t1:u1:c",
            "1:t1:u1:cEXTRA",
            "1:é1:u1:c",
            "0:1:u1:c",
            "1:t1:u1:\n",
        ] {
            assert_eq!(trace.tool_attempt("test.echo", key), Err(Error::Trace), "{key:?}");
        }
        let mut request = ToolTrace::new(trace.clone(), &catalog());
        let mut call: ToolCall = serde_json::from_value(json!({"threadId":"t", "turnId":"u", "callId":"c",
            "tool":"private-model-name", "arguments":{"text":"private argument"}}))
        .unwrap();
        request.request(&RequestId::Number(1), &call).unwrap();
        call.tool = "tool_00".into();
        request.request(&RequestId::Number(1), &call).unwrap();
        call.tool = "test_wait".into();
        request.request(&RequestId::Number(1), &call).unwrap();
        call.tool = "test_echo__c1".into();
        request.request(&RequestId::Number(1), &call).unwrap();
        call.tool = "test_echo".into();
        assert_eq!(
            request.request(&RequestId::Text("x".repeat(257)), &call),
            Err(Error::Decode)
        );
        call.turn_id = "invalid\nturn".into();
        assert_eq!(request.request(&RequestId::Number(1), &call), Err(Error::Decode));
        assert_eq!(std::fs::read(path).unwrap(), b"");
    }

    #[tokio::test(start_paused = true)]
    async fn receipts_portos_ordinals_preserve_attempt_order_and_exact_safe_envelopes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("trace");
        let trace = Trace::open(&path, "api_key").unwrap();
        trace.tool_run(Some("run-1")).unwrap();
        let receipts = PortosReceipts::new(trace, "portos".into());
        receipts.catalog(2).unwrap();
        assert_eq!(receipts.attempt("endpoint_query", "invalid"), Err(Error::Trace));
        let first = receipts.attempt("endpoint_query", "1:t1:u1:a").unwrap();
        let second = receipts.attempt("endpoint_list", "1:t1:u1:b").unwrap();
        assert_eq!((first, second), (1, 2));
        receipts.complete("endpoint_list", second, false).unwrap();
        receipts.complete("endpoint_query", first, true).unwrap();
        let rows: Vec<serde_json::Value> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|row| serde_json::from_str(row).unwrap())
            .collect();
        assert_eq!(
            rows,
            vec![
                json!({"event":"mcp.tools_list.completed","server":"portos","outcome":"success","count":2,"elapsed_ms":0,"provider_auth_mode":"api_key"}),
                json!({"event":"tools/call","name":"endpoint_query","thread_id":"t","turn_id":"u","call_id":"a","run_id":"run-1","elapsed_ms":0,"provider_auth_mode":"api_key"}),
                json!({"event":"tools/call","name":"endpoint_list","thread_id":"t","turn_id":"u","call_id":"b","run_id":"run-1","elapsed_ms":0,"provider_auth_mode":"api_key"}),
                json!({"event":"mcp.tool.completed","server":"portos","name":"endpoint_list","outcome":"error","attempt_count":2,"elapsed_ms":0,"provider_auth_mode":"api_key"}),
                json!({"event":"mcp.tool.completed","server":"portos","name":"endpoint_query","outcome":"success","attempt_count":1,"elapsed_ms":0,"provider_auth_mode":"api_key"}),
            ]
        );
    }
}
