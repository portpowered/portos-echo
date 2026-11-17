//! Correlation-only dispatch receipts. Tool bodies never enter the trace.
use super::Trace;
use crate::Error;
use echo_codex::codex_protocol::{RequestId, ToolCall, TurnInterrupt};
use echo_delegation::codex_dispatch::Catalog;
use serde_json::json;

pub(crate) struct ToolTrace {
    trace: Trace,
    names: Vec<String>,
}

impl ToolTrace {
    pub fn new(trace: Trace, catalog: &Catalog) -> Self {
        Self {
            trace,
            names: catalog
                .tools()
                .iter()
                .filter_map(|tool| catalog.resolve(&tool.name).map(str::to_owned))
                .collect(),
        }
    }

    pub fn interrupt(&mut self, event: &str, turn: &TurnInterrupt) -> Result<(), Error> {
        if !valid(&turn.thread_id) || !valid(&turn.turn_id) {
            return Err(Error::Decode);
        }
        let run = self.trace.active_run.lock().map_err(|_| Error::Trace)?.clone();
        self.trace.record(
            event,
            run.as_deref(),
            json!({"thread_id":turn.thread_id,"turn_id":turn.turn_id}),
        )
    }

    pub fn request(&mut self, id: &RequestId, call: &ToolCall) -> Result<(), Error> {
        let Some(name) = call.tool.strip_prefix("tool_").and_then(|index| {
            let index = index.parse::<usize>().ok()?;
            (call.tool == format!("tool_{index}"))
                .then(|| self.names.get(index))
                .flatten()
        }) else {
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
        let run = self.trace.active_run.lock().map_err(|_| Error::Trace)?.clone();
        self.trace.record(
            "item/tool/call",
            run.as_deref(),
            json!({
                "name":name,"thread_id":call.thread_id,"turn_id":call.turn_id,"call_id":call.call_id,"request_id":id
            }),
        )
    }
}

impl Trace {
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
        let run = self.active_run.lock().map_err(|_| Error::Trace)?.clone();
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
            "tool":"tool_0", "arguments":{"text":"private argument"}}))
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
        call.tool = "tool_0".into();
        assert_eq!(
            request.request(&RequestId::Text("x".repeat(257)), &call),
            Err(Error::Decode)
        );
        call.turn_id = "invalid\nturn".into();
        assert_eq!(request.request(&RequestId::Number(1), &call), Err(Error::Decode));
        assert_eq!(std::fs::read(path).unwrap(), b"");
    }
}
