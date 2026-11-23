//! T3: the Settings API-key route over real loopback HTTP. The durable save
//! requests the settled restart that logs the app server in with the new key.
use super::*;

async fn open_api_key() -> (Fixture, SettingsService) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let text = format!(
        "[echo]\nhost='127.0.0.1'\n[live]\nmode='api_key'\nvoice='cove'\ncredential_file='{}'\n[agent]\n[mcp]\n[screen]\npairing_token_file='absent/pairing'\n[audit]\nlog_dir='absent/logs'\n[home]\ndir='{}'\n",
        root.path().join("live-key").display(),
        home.display()
    );
    fs::write(root.path().join("bridge.toml"), text).unwrap();
    fs::write(home.join("settings.toml"), "").unwrap();
    let config = BridgeConfig::load_effective(root.path().join("bridge.toml")).unwrap();
    let prompts = PromptTextsV1 {
        voice: echo_config::LIVE_INSTRUCTIONS.into(),
        runner: echo_config::RUNNER_INSTRUCTIONS.into(),
    };
    let (owner, send) = SettingsService::open(config, prompts).unwrap();
    let (states, input) = mpsc::channel(32);
    let server = serve_with_settings(
        "127.0.0.1:0".parse().unwrap(),
        Secret::new(TOKEN.into()),
        input,
        Some(send),
    )
    .await
    .unwrap();
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let fixture = Fixture {
        root,
        server,
        states,
        client,
    };
    (fixture, owner)
}

async fn post_key(fixture: &Fixture, key: &str) -> (StatusCode, String) {
    let response = fixture
        .client
        .post(fixture.url("/settings/v2/openai/api-key"))
        .json(&json!({"version":2, "api_key":key}))
        .send()
        .await
        .unwrap();
    (response.status(), response.text().await.unwrap())
}

#[tokio::test]
async fn t3_settings_openai_api_key_save_requests_relogin_and_never_echoes_the_key() {
    let (mut fixture, owner) = open_api_key().await;
    let trace = fixture.root.path().join("trace");
    let owner = owner.with_trace(crate::bridge_trace::Trace::open(&trace, "api_key").unwrap());
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let journey = async {
        let (status, body) = post_key(&fixture, "dummy\u{7}local").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({"version":2,"message":"openai.api_key: expected one line of printable text"})
        );
        let (status, body) = post_key(&fixture, "dummy-local-only\n").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({"version":2,"restart_pending":true})
        );
        let snapshot = fixture
            .client
            .get(fixture.url("/settings/v2"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(!snapshot.contains("dummy-local"));
        stop.send(()).unwrap();
    };
    let ((released, completion), ()) = tokio::join!(
        owner.scope(
            async {
                // The voice generation stops at its settled boundary, then re-execs.
                super::super::super::settings_restart::requested().await;
                stopping.await.unwrap();
                "released"
            },
            || LinkStatusV1::Unconfigured
        ),
        journey
    );
    assert_eq!(released, "released");
    assert!(completion.restart_requested);
    assert_eq!(
        fs::read(fixture.root.path().join("live-key")).unwrap(),
        b"dummy-local-only"
    );
    let recorded = fs::read_to_string(&trace).unwrap();
    assert!(recorded.contains("\"event\":\"settings.reexec_pending\""));
    assert!(!recorded.contains("dummy"));
    fixture.server.shutdown().await.unwrap();
}
