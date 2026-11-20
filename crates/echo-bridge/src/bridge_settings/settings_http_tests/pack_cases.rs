//! T3 catalog and same-writer selection through the real loopback listener.
use super::*;
use serde_json::{Value, json};

fn pack(fixture: &Fixture, id: &str, name: &str, version: &str) {
    let root = fixture.path("screens").join(id);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("pack.json"),
        serde_json::to_vec(&json!({
            "format":1,"name":name,"version":version,"character":{"rest":"rest.png"}
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(root.join("rest.png"), "generated-rest").unwrap();
}

async fn catalog(fixture: &Fixture) -> Value {
    let response = fixture
        .client
        .get(fixture.url("/screen-packs/v1"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

async fn select(fixture: &Fixture, selection: &str) -> (StatusCode, Value) {
    let response = fixture
        .client
        .post(fixture.url("/screen-packs/v1/selection"))
        .json(&json!({"version":1,"screen_pack":selection}))
        .send()
        .await
        .unwrap();
    (response.status(), response.json().await.unwrap())
}

#[tokio::test]
async fn t3_screen_pack_catalog_resolves_valid_templates_and_filters_bad_local_data() {
    let mut fixture = Fixture::open().await;
    let _jobs = fixture.server.install_configured_packs(fixture.path(""), None, vec![]);
    assert_eq!(
        catalog(&fixture).await,
        json!({
            "version":1,"revision":0,"active":null,"installed":[],"jobs":[],"warning":null
        })
    );
    for (id, name, version) in [
        ("template", "template", "2.0.0"),
        ("template@10.0.0-beta.2", "template", "10.0.0-beta.2"),
        ("template@10.0.0-beta.10", "template", "10.0.0-beta.10"),
        ("local", "local", "0.1.0"),
        ("wrong", "another", "1.0.0"),
    ] {
        pack(&fixture, id, name, version);
    }
    fs::create_dir_all(fixture.path("screens/.pack-staging")).unwrap();
    fs::write(fixture.path("screens/.private.sha256"), "not public").unwrap();
    pack(&fixture, "broken", "broken", "1.0.0");
    fs::write(fixture.path("screens/broken/pack.json"), "malformed").unwrap();
    let value = catalog(&fixture).await;
    assert_eq!(value["active"], "template@10.0.0-beta.10");
    assert_eq!(
        value["installed"],
        json!([
            {"id":"local","name":"local","version":"0.1.0",
             "manifest_url":"/screen-packs/v1/local/pack.json","rest_url":"/screen-packs/v1/local/rest.png"},
            {"id":"template","name":"template","version":"2.0.0",
             "manifest_url":"/screen-packs/v1/template/pack.json","rest_url":"/screen-packs/v1/template/rest.png"},
            {"id":"template@10.0.0-beta.10","name":"template","version":"10.0.0-beta.10",
             "manifest_url":"/screen-packs/v1/template@10.0.0-beta.10/pack.json","rest_url":"/screen-packs/v1/template@10.0.0-beta.10/rest.png"},
            {"id":"template@10.0.0-beta.2","name":"template","version":"10.0.0-beta.2",
             "manifest_url":"/screen-packs/v1/template@10.0.0-beta.2/pack.json","rest_url":"/screen-packs/v1/template@10.0.0-beta.2/rest.png"}
        ])
    );
    pack(&fixture, "template@10.0.0", "template", "10.0.0");
    assert_eq!(catalog(&fixture).await["active"], "template@10.0.0");
    assert_eq!(
        select(&fixture, "local").await,
        (StatusCode::OK, json!({"version":1,"restart_pending":false}))
    );
    assert_eq!(catalog(&fixture).await["active"], "local");
    fs::write(fixture.path("screens/local/rest.png"), "edited-rest").unwrap();
    assert_eq!(catalog(&fixture).await["active"], "local");
    fs::remove_file(fixture.path("screens/local/rest.png")).unwrap();
    let value = catalog(&fixture).await;
    assert_eq!(value["active"], Value::Null);
    assert_eq!(value["warning"], "invalid_selection");
    assert_eq!(select(&fixture, "").await.0, StatusCode::OK);
    assert_eq!(catalog(&fixture).await["warning"], Value::Null);
    fixture.close().await;
}

#[tokio::test]
async fn t3_screen_pack_selection_persists_without_restart_and_preserves_pending_voice_save() {
    let mut fixture = Fixture::open().await;
    pack(&fixture, "sample@0.1.0", "sample", "0.1.0");
    let _jobs = fixture.server.install_configured_packs(fixture.path(""), None, vec![]);
    assert_eq!(
        select(&fixture, "sample@0.1.0").await,
        (StatusCode::OK, json!({"version":1,"restart_pending":false}))
    );
    assert_eq!(
        fs::read_to_string(fixture.path("settings.toml")).unwrap(),
        "screen_pack = \"sample@0.1.0\"\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(fixture.path("settings.toml"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert!(!fixture.snapshot().await.restart_pending);
    let bytes = fs::read(fixture.path("settings.toml")).unwrap();
    assert_eq!(select(&fixture, "sample@0.1.0").await.0, StatusCode::OK);
    assert_eq!(fs::read(fixture.path("settings.toml")).unwrap(), bytes);
    for invalid in ["missing", "sample", "../sample@0.1.0", "template", "x'\n[voice]\n"] {
        assert_eq!(
            select(&fixture, invalid).await,
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"version":1,"reason":"invalid_selection"})
            )
        );
        assert_eq!(fs::read(fixture.path("settings.toml")).unwrap(), bytes);
    }
    assert_eq!(
        fixture.patch(json!({"voice":{"session_listen_seconds":30}})).await.0,
        StatusCode::OK
    );
    assert_eq!(
        select(&fixture, "").await,
        (StatusCode::OK, json!({"version":1,"restart_pending":true}))
    );
    let overlay = SettingsOverlay::from_toml_str(&fs::read_to_string(fixture.path("settings.toml")).unwrap()).unwrap();
    assert_eq!(overlay.screen_pack(), Some(""));
    assert_eq!(fixture.snapshot().await.settings.voice.session_listen_seconds, 30);
    assert_eq!(catalog(&fixture).await["active"], Value::Null);
    fixture.close().await;
}

#[tokio::test]
async fn t3_screen_pack_catalog_selection_guards_and_failed_writer_retain_choice() {
    let mut fixture = Fixture::open().await;
    let unmounted = fixture
        .client
        .get(fixture.url("/screen-packs/v1"))
        .send()
        .await
        .unwrap();
    assert_eq!(unmounted.status(), StatusCode::SERVICE_UNAVAILABLE);
    pack(&fixture, "sample", "sample", "0.1.0");
    let _jobs = fixture.server.install_configured_packs(fixture.path(""), None, vec![]);
    pairing_guards(&fixture).await;
    selection_input_guards(&fixture).await;
    assert!(!fixture.path("settings.toml").exists());
    assert_eq!(select(&fixture, "sample").await.0, StatusCode::OK);
    fs::remove_file(fixture.path("settings.toml")).unwrap();
    fs::create_dir(fixture.path("settings.toml")).unwrap();
    let (code, error) = select(&fixture, "").await;
    assert_eq!(code, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error["message"], "Could not save file; previous file retained");
    assert_eq!(catalog(&fixture).await["active"], "sample");
    assert!(!fixture.snapshot().await.restart_pending);
    fixture.server.revoke_pairing().await.unwrap();
    assert_eq!(
        fixture
            .client
            .get(fixture.url("/screen-packs/v1"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    fixture.close().await;
}

async fn pairing_guards(fixture: &Fixture) {
    for route in ["/screen-packs/v1", "/screen-packs/v1/selection"] {
        for query in [
            "",
            "token=",
            "token=wrong",
            "token=synthetic-settings-pair&token=synthetic-settings-pair",
        ] {
            let mut url = reqwest::Url::parse(&fixture.url(route)).unwrap();
            url.set_query(Some(query));
            let response = fixture
                .client
                .request(
                    if route.ends_with("selection") {
                        reqwest::Method::POST
                    } else {
                        reqwest::Method::GET
                    },
                    url,
                )
                .body("not json")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(response.text().await.unwrap(), "");
        }
    }
}

async fn selection_input_guards(fixture: &Fixture) {
    let url = fixture.url("/screen-packs/v1/selection");
    for (body, media, code) in [
        ("{}", "text/plain", StatusCode::UNSUPPORTED_MEDIA_TYPE),
        ("not json", "application/json", StatusCode::BAD_REQUEST),
        (
            r#"{"version":2,"screen_pack":"sample"}"#,
            "application/json",
            StatusCode::BAD_REQUEST,
        ),
        (
            r#"{"version":1,"screen_pack":"sample","extra":true}"#,
            "application/json",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let response = fixture
            .client
            .post(&url)
            .header("content-type", media)
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), code);
    }
    let response = fixture
        .client
        .post(&url)
        .header("origin", "http://127.0.0.1:1")
        .json(&json!({"version":1,"screen_pack":"sample"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = fixture
        .client
        .get(fixture.url("/screen-packs/v1"))
        .header("origin", "http://127.0.0.1:1")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = fixture
        .client
        .post(&url)
        .header("content-type", "application/json")
        .body(" ".repeat(65_537))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

struct Events {
    response: reqwest::Response,
    pending: Vec<u8>,
}

impl Events {
    async fn open(fixture: &Fixture) -> Self {
        let response = fixture.client.get(fixture.url("/events")).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        Self {
            response,
            pending: vec![],
        }
    }

    async fn next(&mut self) -> String {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(end) = self.pending.windows(2).position(|bytes| bytes == b"\n\n") {
                    return String::from_utf8(self.pending.drain(..end + 2).collect()).unwrap();
                }
                self.pending
                    .extend_from_slice(&self.response.chunk().await.unwrap().unwrap());
            }
        })
        .await
        .unwrap()
    }

    async fn reload(&mut self, revision: u64, active: Value) {
        let wire = self.next().await;
        let mut lines = wire.lines();
        assert_eq!(lines.next(), Some("event: screen-pack-reload"));
        let payload: Value = serde_json::from_str(lines.next().unwrap().strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(payload, json!({"version":1,"revision":revision,"active":active}));
        assert_eq!(lines.next(), Some(""));
        assert_eq!(lines.next(), None);
    }
}

// Frame publication is a positive FIFO barrier: a duplicate reload or a reload
// from any preceding no-op/refusal would appear before this exact frame.
async fn frame_barrier(fixture: &Fixture, events: &mut Events, seq: u64) {
    fixture
        .states
        .send(echo_screen_contract::UiState::Listening)
        .await
        .unwrap();
    fixture
        .server
        .flush(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(
        events.next().await,
        format!("data: {{\"seq\":{seq},\"state\":{{\"state\":\"listening\"}}}}\n\n")
    );
}

async fn external_settings(fixture: &Fixture, text: &str, calls: u32) {
    fs::write(fixture.path("settings.toml"), text).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = fixture.snapshot().await;
            assert!(!snapshot.restart_pending);
            if snapshot.settings.agent.max_tool_calls == calls {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(fs::read_to_string(fixture.path("settings.toml")).unwrap(), text);
}

#[tokio::test]
async fn t3_screen_pack_external_settings_hot_selection_no_echo_or_restart() {
    let mut fixture = Fixture::open().await;
    pack(&fixture, "template@1.0.0", "template", "1.0.0");
    pack(&fixture, "local", "local", "1.0.0");
    let _jobs = fixture.server.install_configured_packs(fixture.path(""), None, vec![]);
    let mut events = Events::open(&fixture).await;
    external_settings(&fixture, "screen_pack='local'\n[agent]\nmax_tool_calls=4\n", 4).await;
    events.reload(1, json!("local")).await;
    external_settings(&fixture, "screen_pack='local'\n[agent]\nmax_tool_calls=5\n", 5).await;
    frame_barrier(&fixture, &mut events, 1).await;
    external_settings(&fixture, "screen_pack=''\n[agent]\nmax_tool_calls=6\n", 6).await;
    events.reload(2, Value::Null).await;
    fs::remove_file(fixture.path("settings.toml")).unwrap();
    events.reload(3, json!("template@1.0.0")).await;
    assert_eq!(catalog(&fixture).await["revision"], 3);
    // An HTTP selection followed by the watcher observing that same write
    // still produces one publication; subsequent external adoption is a barrier.
    assert_eq!(select(&fixture, "local").await.0, StatusCode::OK);
    events.reload(4, json!("local")).await;
    external_settings(&fixture, "screen_pack='local'\n[agent]\nmax_tool_calls=3\n", 3).await;
    frame_barrier(&fixture, &mut events, 2).await;
    assert_eq!(catalog(&fixture).await["revision"], 4);
    drop(events);
    fixture.close().await;
}

#[tokio::test]
async fn t3_screen_pack_selection_reload_once_noop_alias_failure_and_reconnect() {
    let mut fixture = Fixture::open().await;
    pack(&fixture, "template@1.0.0", "template", "1.0.0");
    pack(&fixture, "local", "local", "1.0.0");
    let _jobs = fixture.server.install_configured_packs(fixture.path(""), None, vec![]);
    let mut events = Events::open(&fixture).await;
    assert_eq!(catalog(&fixture).await["revision"], 0);
    assert_eq!(select(&fixture, "template@1.0.0").await.0, StatusCode::OK);
    frame_barrier(&fixture, &mut events, 1).await;
    assert_eq!(select(&fixture, "local").await.0, StatusCode::OK);
    events.reload(1, json!("local")).await;
    assert_eq!(catalog(&fixture).await["revision"], 1);
    assert_eq!(select(&fixture, "local").await.0, StatusCode::OK);
    assert_eq!(select(&fixture, "missing").await.0, StatusCode::UNPROCESSABLE_ENTITY);
    frame_barrier(&fixture, &mut events, 2).await;
    assert_eq!(select(&fixture, "template").await.0, StatusCode::OK);
    events.reload(2, json!("template@1.0.0")).await;
    assert_eq!(select(&fixture, "template@1.0.0").await.0, StatusCode::OK);
    frame_barrier(&fixture, &mut events, 3).await;
    assert_eq!(select(&fixture, "").await.0, StatusCode::OK);
    events.reload(3, Value::Null).await;
    fs::remove_file(fixture.path("settings.toml")).unwrap();
    fs::create_dir(fixture.path("settings.toml")).unwrap();
    assert_eq!(select(&fixture, "local").await.0, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(catalog(&fixture).await["revision"], 3);
    assert_eq!(catalog(&fixture).await["active"], Value::Null);
    frame_barrier(&fixture, &mut events, 4).await;
    let mut reconnected = Events::open(&fixture).await;
    assert_eq!(
        reconnected.next().await,
        "data: {\"seq\":4,\"state\":{\"state\":\"listening\"}}\n\n"
    );
    frame_barrier(&fixture, &mut reconnected, 5).await;
    assert!(!fixture.snapshot().await.restart_pending);
    fixture.server.revoke_pairing().await.unwrap();
    // Revocation drops the connection immediately; chunked HTTP need not have
    // a terminal zero chunk. A body/decode EOF is closure, never another event.
    match reconnected.response.chunk().await {
        Ok(chunk) => assert!(chunk.is_none()),
        Err(error) => assert!(error.is_body() || error.is_decode()),
    }
    drop(events);
    drop(reconnected);
    fixture.close().await;
}

#[tokio::test]
async fn t3_screen_pack_settled_active_edits_touch_replacement_and_invalid_fallback() {
    let mut fixture = Fixture::open().await;
    pack(&fixture, "local", "local", "1.0.0");
    pack(&fixture, "other", "other", "1.0.0");
    let _jobs = fixture.server.install_configured_packs(fixture.path(""), None, vec![]);
    let mut events = Events::open(&fixture).await;
    assert_eq!(select(&fixture, "local").await.0, StatusCode::OK);
    events.reload(1, json!("local")).await;
    let settings = fs::read(fixture.path("settings.toml")).unwrap();
    fs::write(fixture.path("screens/other/rest.png"), "inactive-edit").unwrap();
    fs::write(fixture.path("screens/local/rest.png"), "new-rest").unwrap();
    // An alias/no-op save racing the disk observer must not swallow the edit.
    assert_eq!(select(&fixture, "local").await.0, StatusCode::OK);
    events.reload(2, json!("local")).await;
    assert_eq!(catalog(&fixture).await["revision"], 2);
    // A metadata-only touch must reload even though serving bytes are identical.
    let path = fixture.path("screens/local/rest.png");
    let file = fs::File::options().write(true).open(&path).unwrap();
    file.set_times(fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(10)))
        .unwrap();
    events.reload(3, json!("local")).await;
    let replacement = fixture.path("replacement.png");
    fs::write(&replacement, "new-rest").unwrap();
    fs::rename(replacement, &path).unwrap();
    events.reload(4, json!("local")).await;
    // A bad active manifest resolves to built-in; restoring it hot-loads again.
    let manifest = fixture.path("screens/local/pack.json");
    let original = fs::read(&manifest).unwrap();
    fs::write(&manifest, "invalid").unwrap();
    events.reload(5, Value::Null).await;
    fs::write(&manifest, original).unwrap();
    events.reload(6, json!("local")).await;
    // Later changed observations are positive barriers for duplicate events
    // from all earlier polls. Named reloads never advance ordinary frame seq.
    frame_barrier(&fixture, &mut events, 1).await;
    assert_eq!(catalog(&fixture).await["revision"], 6);
    assert_eq!(fs::read(fixture.path("settings.toml")).unwrap(), settings);
    assert!(!fixture.snapshot().await.restart_pending);
    drop(events);
    fixture.close().await;
}
