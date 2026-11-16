use crate::service_support::{CHAT, Fixture, empty, progress, reject};
use serde_json::{Value, json};

#[tokio::test]
async fn f01_f02_f04_real_tls_to_handler_correlates_exact_terminal_once() {
    let fixture = Fixture::new();
    let daemon = fixture.start().await;
    let first = fixture.submit(&daemon).await;
    let second = fixture.submit(&daemon).await;
    assert_ne!(first, second);
    for id in [&first, &second] {
        assert_eq!(fixture.event(&daemon, id).await, progress(id));
        assert_eq!(fixture.event(&daemon, id).await, empty());
    }
    let calls = fixture.calls();
    assert_eq!(calls.len(), 2);
    assert_ne!(calls[0]["argv"][2], calls[1]["argv"][2]);
    for (call, id) in calls.iter().zip([&first, &second]) {
        let argv = call["argv"].as_array().unwrap();
        assert_eq!(argv.len(), 6);
        assert_eq!(argv[0], "send-user-msg");
        assert_eq!(argv[1], "--session-id");
        assert_eq!(
            uuid::Uuid::parse_str(argv[2].as_str().unwrap())
                .unwrap()
                .get_version_num(),
            4
        );
        assert_eq!(argv[3], "--wait");
        assert_eq!(argv[4], "1");
        assert_eq!(
            argv[5],
            format!(
                "Read light\nContext: {{\"target\":\"test-light\"}}\nTask ID: {id}\nWhen you are finished, call the echo.report.result command with task_id {id}, status completed or failed, and a short summary. Do not report success any other way."
            )
        );
        assert_eq!(call["stdin"], "");
    }
    fixture.accepted(&first, "completed", "light is on").await;
    assert_eq!(
        fixture.event(&daemon, &first).await,
        (200, json!({"type":"completed","task_id":first,"summary":"light is on"}))
    );
    assert_eq!(fixture.event(&daemon, &first).await, empty());
    assert_eq!(fixture.event(&daemon, &second).await, empty());
    fixture.rejected(&first, "task_id already reported").await;
    fixture.accepted(&second, "failed", "light is off").await;
    assert_eq!(
        fixture.event(&daemon, &second).await,
        (
            200,
            json!({"type":"failed","task_id":second,"retryable":false,"message":"light is off"})
        )
    );
    assert_eq!(fixture.event(&daemon, &second).await, empty());
    fixture.rejected(&second, "task_id already reported").await;
    assert_eq!(fixture.calls().len(), 2);
    daemon.stop(true).await;
}

#[tokio::test]
async fn f03_auth_precedes_body_lookup_and_mutation() {
    let fixture = Fixture::new();
    let daemon = fixture.start().await;
    let before = std::fs::read(fixture.path("tasks.json")).unwrap();
    for credential in [None, Some("wrong")] {
        for (method, path) in [
            ("POST", "/v1/tasks"),
            ("GET", "/v1/tasks/unknown/events"),
            ("POST", "/v1/tasks/unknown/cancel"),
        ] {
            let mut request = fixture
                .client
                .request(method.parse().unwrap(), format!("{}{path}", daemon.url))
                .body("{");
            if let Some(value) = credential {
                request = request.bearer_auth(value);
            }
            assert_eq!(
                crate::service_support::decode(request.send().await.unwrap()).await,
                (401, json!({"error":"unauthorized"}))
            );
        }
    }
    let mut duplicate = fixture
        .client
        .post(format!("{}/v1/tasks", daemon.url))
        .bearer_auth(&fixture.credential)
        .body("{")
        .build()
        .unwrap();
    duplicate.headers_mut().append(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_static("Bearer wrong"),
    );
    assert_eq!(
        crate::service_support::decode(fixture.client.execute(duplicate).await.unwrap()).await,
        (401, json!({"error":"unauthorized"}))
    );
    assert_eq!(fixture.calls(), Vec::<Value>::new());
    assert_eq!(std::fs::read(fixture.path("tasks.json")).unwrap(), before);
    daemon.stop(true).await;
}

#[tokio::test]
async fn f17_only_task_operations_are_public_and_context_cannot_select_commands() {
    let fixture = Fixture::new();
    let daemon = fixture.start().await;
    for (method, path, body, status, code) in [
        ("POST", "/report", None, 404, "not_found"),
        ("POST", "/v1/report", None, 404, "not_found"),
        ("GET", "/v1/tasks", None, 405, "method_not_allowed"),
        ("POST", "/v1/tasks", Some(json!({})), 400, "invalid_request"),
        ("POST", "/v1/tasks", None, 415, "unsupported_media_type"),
        ("GET", "/v1/tasks/unknown/events", None, 404, "unknown_task"),
    ] {
        assert_eq!(
            fixture.request(&daemon, method, path, body).await,
            (status, json!({"error":code}))
        );
    }
    let response = fixture
        .client
        .post(format!("{}/v1/tasks", daemon.url))
        .bearer_auth(&fixture.credential)
        .header("content-type", "application/json")
        .body("x".repeat(65537))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 413);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"error":"payload_too_large"})
    );
    let mut task = crate::service_support::task();
    task["program"] = json!("inert");
    task["context"] = json!({"command":"system.run","consent":true,"path":"/inert"});
    assert_eq!(fixture.request(&daemon, "POST", "/v1/tasks", Some(task)).await.0, 201);
    assert_eq!(fixture.calls().len(), 1);
    daemon.stop(true).await;
}

#[tokio::test]
async fn f05_invalid_callbacks_preserve_live_task() {
    let fixture = Fixture::new();
    let daemon = fixture.start().await;
    let id = fixture.submit(&daemon).await;
    fixture.rejected("unknown", "unknown task_id").await;
    for (input, error) in [
        ("{", "invalid report: EOF while parsing an object at line 1 column 1"),
        (
            r#"{"task_id":"x","status":"completed","summary":"x","extra":true}"#,
            "invalid report: unknown field `extra`, expected one of `task_id`, `status`, `summary` at line 1 column 57",
        ),
        (
            r#"{"task_id":"x","status":"success","summary":"x"}"#,
            "invalid report: unknown variant `success`, expected `completed` or `failed` at line 1 column 33",
        ),
    ] {
        reject(&fixture.report(input.as_bytes()).await, error);
    }
    assert_eq!(fixture.event(&daemon, &id).await, progress(&id));
    fixture.accepted(&id, "completed", "unchanged").await;
    daemon.stop(true).await;
}

#[tokio::test]
async fn f06_f07_controlled_deadline_and_cancel_reject_late_results() {
    let fixture = Fixture::new();
    let mut daemon = fixture.start().await;
    let expired = fixture.submit(&daemon).await;
    assert_eq!(fixture.event(&daemon, &expired).await, progress(&expired));
    daemon.time(159).await;
    assert_eq!(fixture.event(&daemon, &expired).await, empty());
    let pending_request = fixture
        .client
        .get(format!("{}/v1/tasks/{expired}/events", daemon.url))
        .bearer_auth(&fixture.credential)
        .send();
    tokio::pin!(pending_request);
    tokio::select! {
        biased;
        result = &mut pending_request => panic!("event returned before deadline: {result:?}"),
        () = tokio::task::yield_now() => {},
    }
    daemon.time(160).await;
    assert_eq!(
        crate::service_support::decode(pending_request.await.unwrap()).await,
        (
            200,
            json!({"type":"failed","task_id":expired,"retryable":false,"message":"Muse did not report a result in time"})
        )
    );
    assert_eq!(fixture.event(&daemon, &expired).await, empty());
    fixture.rejected(&expired, "expired task_id").await;
    assert_eq!(fixture.calls().len(), 1);
    let cancelled = fixture.submit(&daemon).await;
    assert_eq!(
        fixture
            .request(&daemon, "POST", &format!("/v1/tasks/{cancelled}/cancel"), None)
            .await,
        empty()
    );
    assert_eq!(fixture.event(&daemon, &cancelled).await, empty());
    fixture.rejected(&cancelled, "cancelled task_id").await;
    let calls = fixture.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[1]["argv"][2], calls[2]["argv"][2]);
    assert_eq!(
        calls[2]["argv"][5],
        format!("Cancel task {cancelled}. Do not call echo.report.result for it.")
    );
    assert!(calls[0]["argv"][5].as_str().unwrap().starts_with("Read light"));
    assert_eq!(CHAT, "Done! The job completed successfully.");
    daemon.stop(true).await;
}
