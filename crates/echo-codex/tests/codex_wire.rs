//! T2: selected synthetic envelopes from the pinned generated experimental schema.
use echo_codex::{
    codex_codec::encode,
    codex_protocol::{self as wire, Message, RequestId},
};
use echo_contracts::secret::Secret;
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

#[test]
fn selected_sign_in_shapes_match_the_pin() -> Result<(), Box<dyn std::error::Error>> {
    let key = Secret::new("dummy-local-only".to_owned());
    let encoded = encode(&wire::LoginParams::ApiKey { api_key: &key }, 128)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&encoded)?,
        json!({"type":"apiKey","apiKey":"dummy-local-only"})
    );
    assert_eq!(
        serde_json::to_value(wire::LoginParams::ChatgptDeviceCode {})?,
        json!({"type":"chatgptDeviceCode"})
    );
    assert_eq!(
        serde_json::to_value(wire::AccountRead { refresh_token: None })?,
        json!({})
    );
    assert_eq!(
        serde_json::to_value(wire::AccountRead {
            refresh_token: Some(false)
        })?,
        json!({"refreshToken":false})
    );
    assert!(matches!(
        serde_json::from_value::<wire::LoginResult>(json!({"type":"apiKey"}))?,
        wire::LoginResult::ApiKey
    ));
    let wire::LoginResult::ChatgptDeviceCode {
        login_id,
        verification_url,
        user_code,
    } = serde_json::from_value(
        json!({"type":"chatgptDeviceCode","loginId":"login-local","verificationUrl":"http://127.0.0.1:9/activate","userCode":"synthetic-code"}),
    )?
    else {
        return Err("device-code reply expected".into());
    };
    assert_eq!(login_id, "login-local");
    assert_eq!(verification_url, "http://127.0.0.1:9/activate");
    assert_eq!(user_code.expose(), "synthetic-code");
    Ok(())
}

#[test]
fn account_state_and_thread_identifier_are_projected_from_replies() -> Result<(), Box<dyn std::error::Error>> {
    let signed_out: wire::AccountReadResult =
        serde_json::from_value(json!({"account":null,"requiresOpenaiAuth":true}))?;
    assert!(signed_out.account.is_none());
    assert!(signed_out.requires_openai_auth);
    assert!(signed_out.workspace_routing.is_none());
    let signed_in: wire::AccountReadResult = serde_json::from_value(
        json!({"account":{"type":"apiKey"},"requiresOpenaiAuth":false,"workspaceRouting":null}),
    )?;
    assert!(matches!(signed_in.account, Some(wire::Account::ApiKey)));
    assert!(!signed_in.requires_openai_auth);
    let account: wire::Account = serde_json::from_value(json!({"type":"chatgpt","email":null,"planType":"unknown"}))?;
    let wire::Account::Chatgpt { email, plan_type } = account else {
        return Err("chatgpt account expected".into());
    };
    assert!(email.is_none());
    assert_eq!(plan_type, "unknown");
    let thread: wire::ThreadStartResult =
        serde_json::from_value(json!({"thread":{"id":"thread-local"},"future":true}))?;
    assert_eq!(thread.thread.id, "thread-local");
    Ok(())
}

fn known(method: &str, params: Value) -> Result<wire::KnownNotification, Box<dyn std::error::Error>> {
    let mut envelope = json!({"method":method});
    envelope["params"] = params;
    let frame = serde_json::to_vec(&envelope)?;
    let Message::Notification { raw, .. } = wire::decode(&frame)? else {
        return Err("notification expected".into());
    };
    Ok(serde_json::from_value(raw)?)
}

#[test]
fn realtime_start_sdp_and_opaque_item_are_projected() -> Result<(), Box<dyn std::error::Error>> {
    use wire::KnownNotification as Event;
    let Event::Started(start) = known(
        "thread/realtime/started",
        json!({"threadId":"thread-local","version":"v3","realtimeSessionId":null}),
    )?
    else {
        return Err("started expected".into());
    };
    assert_eq!(start.thread_id, "thread-local");
    assert_eq!(start.version, "v3");
    assert!(start.realtime_session_id.is_none());
    let Event::Sdp(sdp) = known(
        "thread/realtime/sdp",
        json!({"threadId":"thread-local","sdp":"v=0\r\n"}),
    )?
    else {
        return Err("sdp expected".into());
    };
    assert_eq!(sdp.thread_id, "thread-local");
    assert_eq!(sdp.sdp, "v=0\r\n");
    let Event::ItemAdded(item) = known(
        "thread/realtime/itemAdded",
        json!({"threadId":"thread-local","item":{"type":"speech_started","future":7}}),
    )?
    else {
        return Err("item expected".into());
    };
    assert_eq!(item.thread_id, "thread-local");
    assert_eq!(item.item, json!({"type":"speech_started","future":7}));
    Ok(())
}

#[test]
fn transcripts_errors_and_close_preserve_exact_thread_text_role_and_reason() -> Result<(), Box<dyn std::error::Error>> {
    use wire::KnownNotification as Event;
    let Event::TranscriptDelta(delta) = known(
        "thread/realtime/transcript/delta",
        json!({"threadId":"thread-local","role":"assistant","delta":"echo-"}),
    )?
    else {
        return Err("delta expected".into());
    };
    assert_eq!(delta.thread_id, "thread-local");
    assert_eq!(delta.role, "assistant");
    assert_eq!(delta.delta, "echo-");
    let Event::TranscriptDone(done) = known(
        "thread/realtime/transcript/done",
        json!({"threadId":"thread-local","role":"assistant","text":"echo-ok"}),
    )?
    else {
        return Err("done expected".into());
    };
    assert_eq!(done.thread_id, "thread-local");
    assert_eq!(done.role, "assistant");
    assert_eq!(done.text, "echo-ok");
    let Event::Error(error) = known(
        "thread/realtime/error",
        json!({"threadId":"thread-local","message":"synthetic outage"}),
    )?
    else {
        return Err("error expected".into());
    };
    assert_eq!(error.thread_id, "thread-local");
    assert_eq!(error.message, "synthetic outage");
    let Event::Closed(closed) = known(
        "thread/realtime/closed",
        json!({"threadId":"thread-local","reason":"stopped"}),
    )?
    else {
        return Err("closed expected".into());
    };
    assert_eq!(closed.thread_id, "thread-local");
    assert_eq!(closed.reason.as_deref(), Some("stopped"));
    let Event::Closed(closed) = known("thread/realtime/closed", json!({"threadId":"thread-local"}))? else {
        return Err("closed expected".into());
    };
    assert!(closed.reason.is_none());
    Ok(())
}

#[test]
fn login_notifications_expose_completion_and_relink_failure_without_diagnostic_logging()
-> Result<(), Box<dyn std::error::Error>> {
    use wire::KnownNotification as Event;
    let Event::LoginCompleted(done) = known(
        "account/login/completed",
        json!({"loginId":"login-local","success":false,"error":"synthetic expired code","onboardingEntrypoint":null}),
    )?
    else {
        return Err("login completion expected".into());
    };
    assert_eq!(done.login_id.as_deref(), Some("login-local"));
    assert!(!done.success);
    assert_eq!(done.error.as_deref(), Some("synthetic expired code"));
    assert!(done.onboarding_entrypoint.is_none());
    let Event::AccountUpdated(account) = known("account/updated", json!({"authMode":"apikey","planType":null}))? else {
        return Err("account update expected".into());
    };
    assert_eq!(account.auth_mode.as_deref(), Some("apikey"));
    assert!(account.plan_type.is_none());
    for (method, params) in [
        ("thread/realtime/sdp", json!({"threadId":"thread-local","sdp":9})),
        (
            "thread/realtime/transcript/delta",
            json!({"threadId":"thread-local","role":"user"}),
        ),
        ("account/login/completed", json!({"success":"false"})),
    ] {
        assert!(
            known(method, params).is_err(),
            "malformed {method} must fail its typed projection"
        );
    }
    Ok(())
}
