use super::*;

fn emotion_followup() -> Value {
    let mut request = request();
    let input = request["input"].as_array_mut().unwrap();
    input.push(super::super::emotion::call());
    let output =
        json!({"content":[{"type":"text","text":super::super::emotion::state().to_string()}],"is_error":false});
    input.push(json!({"type":"function_call_output","call_id":"call-1","output":output.to_string()}));
    request
}

#[test]
fn m6_model_requires_the_exact_actual_emotion_result_and_catalog_identity() {
    assert_eq!(
        response_for(&request(), 0, false, true),
        Ok(super::super::emotion::call())
    );
    let message = response_for(&emotion_followup(), 1, false, true).unwrap();
    assert_eq!(message["content"][0]["text"], super::super::emotion::REPLY);
    for output in [
        json!({"content":[],"is_error":false}),
        json!({"content":[],"is_error":true}),
    ] {
        let mut wrong = emotion_followup();
        wrong["input"][3]["output"] = json!(output.to_string());
        assert_eq!(
            response_for(&wrong, 1, false, true),
            Err("model: exact tool output mismatch")
        );
    }
    assert_eq!(
        response_for(&followup(false), 1, false, true),
        Err("model: function call identity mismatch")
    );
}

#[tokio::test]
#[ignore = "T3 loopback controlled Responses peer"]
async fn m6_t3_model_holds_terminal_and_validation_until_native_animation_observer_releases() {
    let (release, held) = oneshot::channel();
    let model = Model::bind_controlled(false, async { Ok(()) }, Some(held))
        .await
        .unwrap();
    let first = post(&model.origin, request()).await;
    assert!(first.contains("tool_3"));
    let origin = model.origin.clone();
    let terminal = tokio::spawn(async move { post(&origin, emotion_followup()).await });
    let mut observed = model.observed.clone();
    observed.wait_for(|o| o.requests == 2).await.unwrap();
    assert!(!terminal.is_finished());
    assert!(!observed.borrow().output_verified);
    release.send(()).unwrap();
    let reply = terminal.await.unwrap();
    assert!(reply.contains(super::super::emotion::REPLY));
    observed.wait_for(|o| o.output_verified).await.unwrap();
    assert_eq!(model.check(), Ok(()));
    model.close().await.unwrap();
}

fn request() -> Value {
    json!({"model":"fixture-model","stream":true,"store":false,"parallel_tool_calls":false,
        "tools":tools().unwrap(),"input":[{"role":"developer"},{"role":"user"}]})
}

fn followup(negative: bool) -> Value {
    let mut request = request();
    let input = request["input"].as_array_mut().unwrap();
    input.push(call());
    let result = if negative {
        json!({"content":{"error":"server error: http_status:500"},"is_error":true}) // lint-rules: allow-literal inert tool error text, not a network address
    } else {
        json!({"content":[{"type":"text","text":"delegated echo"}],"is_error":false})
    };
    input.push(json!({"type":"function_call_output","call_id":"call-1","output":result.to_string()}));
    request
}

#[test]
fn first_turn_and_exact_positive_negative_results_release_only_matching_commentary() {
    assert_eq!(response(&request(), 0, false), Ok(call()));
    for negative in [false, true] {
        let output = response(&followup(negative), 1, negative).unwrap();
        assert_eq!(
            output,
            json!({"type":"message","role":"assistant","content":[{
            "type":"output_text","text":if negative { FALLBACK } else { SUCCESS }}]})
        );
        assert_eq!(
            response(&followup(!negative), 1, negative),
            Err("model: exact tool output mismatch")
        );
    }
}

#[test]
fn wrong_identity_bytes_history_and_retries_cannot_release_success() {
    for replacement in [json!("{}"), json!(null), json!("not-json")] {
        let mut wrong = followup(false);
        wrong["input"][3]["output"] = replacement;
        assert!(response(&wrong, 1, false).is_err());
    }
    for (index, field, value) in [
        (3, "call_id", json!("stale")),
        (3, "type", json!("message")),
        (2, "call_id", json!("stale")),
        (2, "arguments", json!("{\"text\":\"wrong\"}")),
    ] {
        let mut wrong = followup(false);
        wrong["input"][index][field] = value;
        assert_eq!(
            response(&wrong, 1, false),
            Err("model: function call identity mismatch")
        );
    }
    let mut wrong = followup(false);
    wrong["input"].as_array_mut().unwrap().push(json!({}));
    assert_eq!(response(&wrong, 1, false), Err("model: input history mismatch"));
    assert_eq!(
        response(&followup(false), 2, false),
        Err("model: unexpected extra request")
    );
}

#[test]
fn model_contract_requires_streaming_nonparallel_private_requests() {
    for (field, value) in [
        ("model", json!("wrong")),
        ("store", json!(true)),
        ("stream", json!(false)),
        ("parallel_tool_calls", json!(true)),
        ("tools", json!([])),
        ("input", json!([])),
    ] {
        let mut wrong = request();
        wrong[field] = value;
        assert!(response(&wrong, 0, false).is_err());
    }
}

async fn post(origin: &str, body: Value) -> String {
    let mut socket = TcpStream::connect(origin.strip_prefix("http://").unwrap())
        .await
        .unwrap();
    let body = body.to_string();
    socket.write_all(format!("POST /v1/responses HTTP/1.1\r\nHost: local\r\nAuthorization: Bearer synthetic-m4-key\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).await.unwrap();
    response
}

#[tokio::test]
#[ignore = "T3 loopback Responses peer"]
async fn t3_model_matches_wire_results_rejects_retry_and_closes_listener() {
    for negative in [false, true] {
        let model = Model::bind(negative).await.unwrap();
        let origin = model.origin.clone();
        assert_eq!(
            model.check(),
            Err("model: expected exactly two requests and verified tool output".into())
        );
        let first = post(&origin, request()).await;
        assert!(first.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(
            first.contains(&json!({"type":"response.output_item.done","output_index":0,"item":call()}).to_string())
        );
        let second = post(&origin, followup(negative)).await;
        assert!(second.contains(if negative { FALLBACK } else { SUCCESS }));
        // The completed response is the barrier; wait on the owner's observation.
        let mut observed = model.observed.clone();
        observed.wait_for(|v| v.output_verified).await.unwrap();
        assert_eq!(model.check(), Ok(()));
        assert_eq!(post(&origin, followup(negative)).await, "");
        observed.wait_for(|v| v.failure.is_some()).await.unwrap();
        assert_eq!(model.check(), Err("model: unexpected extra request".into()));
        model.close().await.unwrap();
        // post read EOF on the owned retry connection. close joins the listener
        // task; its observation sender must now be dropped, without probing a
        // released ephemeral address that another parallel test may own.
        assert!(observed.has_changed().is_err());
    }
}

#[tokio::test]
#[ignore = "T3 loopback Responses peer"]
async fn t3_model_wrong_result_never_emits_commentary_and_stalled_body_is_collected() {
    let model = Model::bind(true).await.unwrap();
    post(&model.origin, request()).await;
    assert_eq!(post(&model.origin, followup(false)).await, "");
    let mut observed = model.observed.clone();
    observed.wait_for(|v| v.failure.is_some()).await.unwrap();
    assert_eq!(model.check(), Err("model: exact tool output mismatch".into()));
    model.close().await.unwrap();
    let model = Model::bind(false).await.unwrap();
    let mut socket = TcpStream::connect(model.origin.strip_prefix("http://").unwrap())
        .await
        .unwrap();
    socket
        .write_all(
            b"POST /v1/responses HTTP/1.1\r\nContent-Length: 64\r\nAuthorization: Bearer synthetic-m4-key\r\n\r\n{",
        )
        .await
        .unwrap();
    let mut observed = model.observed.clone();
    observed.wait_for(|v| v.requests == 1).await.unwrap();
    model.close().await.unwrap();
    assert!(observed.has_changed().is_err());
    match socket.read(&mut [0]).await {
        Ok(count) => assert_eq!(count, 0),
        Err(error) => assert!(matches!(
            error.kind(),
            std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
        )),
    }
}
