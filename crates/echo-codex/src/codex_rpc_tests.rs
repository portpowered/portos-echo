use super::*;

fn state() -> Result<RpcState, RpcError> {
    RpcState::new(Limits {
        line_bytes: 256,
        pending_requests: 2,
        notifications: 2,
        request_timeout: Duration::from_secs(10),
    })
}

#[test]
fn replies_correlate_out_of_order_and_admission_includes_unread_completions() -> Result<(), RpcError> {
    let mut rpc = state()?;
    let (a, _) = rpc.request("a", &json!({}), Duration::ZERO)?;
    let (b, _) = rpc.request("b", &json!({}), Duration::ZERO)?;
    rpc.receive(
        Message::Response {
            id: b.clone(),
            result: json!("second"),
        },
        Duration::ZERO,
    )?;
    assert_eq!(rpc.request("c", &(), Duration::ZERO).err(), Some(RpcError::PendingFull));
    assert_eq!(rpc.take_reply(&a)?, None);
    assert_eq!(rpc.take_reply(&b)?, Some(Ok(json!("second"))));
    rpc.receive(
        Message::Response {
            id: a.clone(),
            result: json!("first"),
        },
        Duration::ZERO,
    )?;
    assert_eq!(rpc.take_reply(&a)?, Some(Ok(json!("first"))));
    assert_eq!(rpc.take_reply(&a), Err(RpcError::UnknownRequest));
    Ok(())
}

#[test]
fn expired_and_cancelled_requests_never_accept_late_replies() -> Result<(), RpcError> {
    let mut rpc = state()?;
    let (a, _) = rpc.request("a", &(), Duration::ZERO)?;
    let (b, _) = rpc.request("b", &(), Duration::from_secs(1))?;
    assert_eq!(rpc.next_deadline(), Some(Duration::from_secs(10)));
    rpc.expire(Duration::from_secs(9));
    assert_eq!(rpc.take_reply(&a)?, None);
    rpc.receive(
        Message::Response {
            id: a.clone(),
            result: json!("too late"),
        },
        Duration::from_secs(10),
    )?;
    assert_eq!(rpc.take_reply(&a)?, Some(Err(RpcError::Deadline)));
    assert!(rpc.cancel(&b));
    assert!(!rpc.cancel(&b));
    rpc.receive(
        Message::Response {
            id: b.clone(),
            result: json!("cancelled"),
        },
        Duration::from_secs(10),
    )?;
    assert_eq!(rpc.take_reply(&b), Err(RpcError::UnknownRequest));
    let (c, _) = rpc.request("c", &(), Duration::from_secs(10))?;
    assert_eq!(c, RequestId::Number(3));
    assert_eq!(rpc.next_deadline(), Some(Duration::from_secs(20)));
    Ok(())
}

fn notification(value: u8) -> Message {
    Message::Notification {
        method: "future/event".into(),
        params: json!({"opaque": value}),
        raw: json!({"method": "future/event", "params": {"opaque": value}}),
    }
}

#[test]
fn event_overflow_fails_pending_but_preserves_every_accepted_event() -> Result<(), RpcError> {
    let mut rpc = state()?;
    let (id, _) = rpc.request("pending", &(), Duration::ZERO)?;
    rpc.receive(notification(1), Duration::ZERO)?;
    rpc.receive(notification(2), Duration::ZERO)?;
    assert_eq!(rpc.receive(notification(3), Duration::ZERO), Err(RpcError::EventsFull));
    assert_eq!(rpc.take_reply(&id)?, Some(Err(RpcError::EventsFull)));
    assert_eq!(
        rpc.request("new", &(), Duration::ZERO).err(),
        Some(RpcError::EventsFull)
    );
    for expected in [1, 2] {
        let Some(Message::Notification { method, params, .. }) = rpc.next_event() else {
            return Err(RpcError::Malformed);
        };
        assert_eq!(method, "future/event");
        assert_eq!(params, json!({"opaque": expected}));
    }
    assert!(rpc.next_event().is_none());
    Ok(())
}

#[test]
fn server_string_ids_are_answered_exactly_once_and_bounded_after_consumption() -> Result<(), RpcError> {
    let mut rpc = state()?;
    for id in ["tool-a", "tool-b"] {
        rpc.receive(
            Message::Request {
                id: RequestId::Text(id.into()),
                method: "item/tool/call".into(),
                params: json!({"tool": "echo_test"}),
            },
            Duration::ZERO,
        )?;
        assert!(matches!(rpc.next_event(), Some(Message::Request { .. })));
    }
    let id = RequestId::Text("tool-a".into());
    let response = rpc.respond(&id, &json!({"success": true}))?;
    assert_eq!(
        serde_json::from_slice::<Value>(&response).map_err(|_| RpcError::Malformed)?,
        json!({"id": "tool-a", "result": {"success": true}})
    );
    assert_eq!(rpc.respond(&id, &()), Err(RpcError::UnknownRequest));
    rpc.receive(
        Message::Request {
            id: RequestId::Number(-7),
            method: "x".into(),
            params: Value::Null,
        },
        Duration::ZERO,
    )?;
    rpc.next_event();
    assert_eq!(
        rpc.receive(
            Message::Request {
                id: RequestId::Number(-8),
                method: "x".into(),
                params: Value::Null
            },
            Duration::ZERO
        ),
        Err(RpcError::PendingFull)
    );
    assert_eq!(
        rpc.respond(&RequestId::Text("tool-b".into()), &()),
        Err(RpcError::PendingFull)
    );
    Ok(())
}

#[test]
fn remote_error_and_disconnect_never_expose_remote_text_or_overwrite_completion() -> Result<(), RpcError> {
    let mut rpc = state()?;
    let (a, _) = rpc.request("a", &(), Duration::ZERO)?;
    let (b, _) = rpc.request("b", &(), Duration::ZERO)?;
    rpc.receive(
        Message::Failure {
            id: a.clone(),
            code: -32601,
            retryable: false,
        },
        Duration::ZERO,
    )?;
    rpc.disconnect(RpcError::Disconnected);
    rpc.disconnect(RpcError::Malformed);
    assert_eq!(rpc.take_reply(&a)?, Some(Err(RpcError::Remote(-32601))));
    assert_eq!(rpc.take_reply(&b)?, Some(Err(RpcError::Disconnected)));
    assert_eq!(rpc.notify("initialized"), Err(RpcError::Disconnected));
    Ok(())
}

#[test]
fn outgoing_overflow_does_not_consume_admission_or_server_reply() -> Result<(), RpcError> {
    let mut rpc = state()?;
    assert_eq!(
        rpc.request("x", &"x".repeat(257), Duration::ZERO).err(),
        Some(RpcError::LineTooLong)
    );
    let (id, frame) = rpc.request("initialize", &json!({}), Duration::ZERO)?;
    assert_eq!(id, RequestId::Number(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&frame).map_err(|_| RpcError::Malformed)?,
        json!({"id": 1, "method": "initialize", "params": {}})
    );
    let tool = RequestId::Text("tool".into());
    rpc.receive(
        Message::Request {
            id: tool.clone(),
            method: "item/tool/call".into(),
            params: Value::Null,
        },
        Duration::ZERO,
    )?;
    assert_eq!(rpc.respond(&tool, &"x".repeat(257)), Err(RpcError::LineTooLong));
    assert!(rpc.respond(&tool, &()).is_ok());
    Ok(())
}

#[test]
fn invalid_limits_duplicate_ids_and_deadline_overflow_are_explicit() -> Result<(), RpcError> {
    for limits in [
        Limits {
            line_bytes: 0,
            ..Limits::default()
        },
        Limits {
            pending_requests: 0,
            ..Limits::default()
        },
        Limits {
            notifications: 0,
            ..Limits::default()
        },
        Limits {
            request_timeout: Duration::ZERO,
            ..Limits::default()
        },
    ] {
        assert!(matches!(RpcState::new(limits), Err(RpcError::InvalidLimits)));
    }
    let mut rpc = state()?;
    assert_eq!(
        rpc.request("x", &(), Duration::MAX).err(),
        Some(RpcError::InvalidLimits)
    );
    for expected in [Ok(()), Err(RpcError::Malformed)] {
        assert_eq!(
            rpc.receive(
                Message::Request {
                    id: RequestId::Number(1),
                    method: "x".into(),
                    params: Value::Null
                },
                Duration::ZERO
            ),
            expected
        );
    }
    Ok(())
}

#[test]
fn exhausted_ids_refuse_admission_and_completed_replies_ignore_duplicates() -> Result<(), RpcError> {
    let mut rpc = state()?;
    let (id, _) = rpc.request("a", &(), Duration::ZERO)?;
    rpc.receive(
        Message::Response {
            id: id.clone(),
            result: json!(1),
        },
        Duration::ZERO,
    )?;
    rpc.receive(
        Message::Response {
            id: id.clone(),
            result: json!(2),
        },
        Duration::ZERO,
    )?;
    assert_eq!(rpc.take_reply(&id)?, Some(Ok(json!(1))));
    rpc.next_id = i64::MAX;
    assert_eq!(rpc.request("x", &(), Duration::ZERO).err(), Some(RpcError::IdExhausted));
    assert_eq!(rpc.next_deadline(), None);
    Ok(())
}

#[test]
fn startup_network_exact_pin_envelopes_discard_payloads_and_preserve_unknown_errors() {
    use crate::codex_protocol::decode;
    for (code, message, expected) in [
        (-32603, "workspace routing discovery failed", true),
        (-32603, "workspace routing discovery timed out", true),
        (-32603, "workspace routing discovery unauthorized (401)", false),
        (-32603, "workspace routing discovery failed: private detail", false),
        (-32603, "unknown", false),
        (-32000, "workspace routing discovery failed", false),
    ] {
        let mut rpc = state().unwrap();
        let (id, _) = rpc.request("account/read", &(), Duration::ZERO).unwrap();
        let envelope = json!({"id":id,"error":{"code":code,"message":message,
            "data":{"private":"synthetic-private-detail"}}});
        rpc.receive(decode(&serde_json::to_vec(&envelope).unwrap()).unwrap(), Duration::ZERO)
            .unwrap();
        let result = rpc.take_reply(&id).unwrap().unwrap().unwrap_err();
        assert_eq!(
            result,
            if expected {
                RpcError::RemoteTransient(code)
            } else {
                RpcError::Remote(code)
            }
        );
        assert!(!result.to_string().contains("private"));
    }
    for error in [
        json!({"code":-32603}),
        json!({"code":-32603,"message":null}),
        json!({"code":"-32603","message":"workspace routing discovery failed"}),
    ] {
        assert!(matches!(
            decode(&serde_json::to_vec(&json!({"id":1,"error":error})).unwrap()),
            Err(RpcError::Malformed)
        ));
    }
}
