//! T2: selected synthetic envelopes from the pinned generated experimental schema.
use echo_codex::{
    codex_codec::encode,
    codex_protocol::{self as wire, Message, RequestId},
};
use serde_json::{Value, json};

#[test]
fn selected_request_envelopes_have_exact_names_and_explicit_v3() -> Result<(), Box<dyn std::error::Error>> {
    let start = wire::RealtimeStart::new("thread-local", "v=0\r\n");
    assert_eq!(
        serde_json::to_value(start)?,
        json!({
            "threadId": "thread-local", "outputModality": "audio",
            "transport": {"type": "webrtc", "sdp": "v=0\r\n"}, "version": "v3",
            "includeStartupContext": false, "clientManagedHandoffs": false
        })
    );
    assert_eq!(
        wire::initialize_params(),
        json!({
            "clientInfo": {"name": "portos_echo", "title": "Port OS Echo", "version": "0.1.0"},
            "capabilities": {"experimentalApi": true}
        })
    );
    let interrupt = wire::TurnInterrupt {
        thread_id: "thread-local".into(),
        turn_id: "turn-local".into(),
    };
    assert_eq!(
        serde_json::to_value(interrupt)?,
        json!({"threadId": "thread-local", "turnId": "turn-local"})
    );
    assert_eq!(
        serde_json::to_value(wire::ThreadId {
            thread_id: "thread-local".into()
        })?,
        json!({"threadId": "thread-local"})
    );
    Ok(())
}

#[test]
fn tool_request_keeps_string_id_namespace_and_arbitrary_arguments() -> Result<(), Box<dyn std::error::Error>> {
    let frame = br#"{"id":"tool-request-1","method":"item/tool/call","params":{"threadId":"thread-local","turnId":"turn-local","callId":"tool-local","tool":"echo_test","arguments":{"message":"ping"},"namespace":null}}"#;
    let Message::Request { id, method, params } = wire::decode(frame)? else {
        return Err("request expected".into());
    };
    assert_eq!(id, RequestId::Text("tool-request-1".into()));
    assert_eq!(method, "item/tool/call");
    let tool: wire::ToolCall = serde_json::from_value(params.clone())?;
    assert_eq!(tool.call_id, "tool-local");
    assert_eq!(tool.arguments, json!({"message": "ping"}));
    assert!(tool.namespace.is_none());
    assert_eq!(serde_json::to_value(tool)?, params);
    let result: wire::ToolResult =
        serde_json::from_value(json!({"contentItems": [{"type": "inputText", "text": "echo-ok"}], "success": true}))?;
    let encoded = encode(&json!({"id": id, "result": result}), 1024)?;
    let value: Value = serde_json::from_slice(&encoded)?;
    assert_eq!(
        value,
        json!({"id":"tool-request-1", "result":{"contentItems":[{"type":"inputText","text":"echo-ok"}],"success":true}})
    );
    assert!(value.get("jsonrpc").is_none());
    Ok(())
}

#[test]
fn unknown_notifications_and_null_results_are_preserved() -> Result<(), Box<dyn std::error::Error>> {
    let Message::Notification { method, params, raw } = wire::decode(
        br#"{"method":"future/notification","params":{"opaque":{"value":7}},"futureMetadata":[true,null]}"#,
    )?
    else {
        return Err("notification expected".into());
    };
    assert_eq!(method, "future/notification");
    assert_eq!(params, json!({"opaque":{"value":7}}));
    assert_eq!(
        raw,
        json!({"method":"future/notification", "params":{"opaque":{"value":7}}, "futureMetadata":[true,null]})
    );
    for id in [json!(-7), json!("response-local")] {
        let frame = serde_json::to_vec(&json!({"id": id, "result": null}))?;
        let Message::Response { id: actual, result } = wire::decode(&frame)? else {
            return Err("response expected".into());
        };
        assert_eq!(serde_json::to_value(actual)?, id);
        assert_eq!(result, Value::Null);
    }
    let Message::Failure { code, .. } = wire::decode(
        br#"{"id":99,"error":{"code":-32601,"message":"synthetic private text","data":{"private":true}}}"#,
    )?
    else {
        return Err("failure expected".into());
    };
    assert_eq!(code, -32601);
    Ok(())
}
