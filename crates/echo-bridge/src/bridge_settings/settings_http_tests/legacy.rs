use super::*;

#[tokio::test]
async fn t3_settings_legacy_patches_keep_v1_storage_and_skip_v2_writes() {
    use serde_json::{Value, json};
    let fixture = Fixture::open().await;
    let path = fixture.path("settings.toml");
    let url = fixture.url("/settings/v2");
    let legacy = json!({"version":2,"settings":{"agent":{"max_tool_calls":2,"task_deadline_seconds":5}}});
    let patch = || fixture.client.patch(&url).json(&legacy).send();
    for persisted in [false, true] {
        let before = fs::read(&path).ok();
        let reply = patch().await.unwrap();
        assert_eq!(reply.status(), StatusCode::OK);
        assert_eq!(
            reply.json::<Value>().await.unwrap(),
            json!({"version":2,"restart_pending":false})
        );
        assert_eq!(fs::read(&path).ok(), before);
        assert_eq!(path.exists(), persisted);
        if !persisted {
            assert_eq!(
                fixture.patch(json!({"agent":{"max_tool_calls":4}})).await,
                (StatusCode::OK, json!({"version":1,"restart_pending":false}))
            );
        }
    }
    assert_eq!(fixture.snapshot().await.settings.agent.max_tool_calls, 4);
    let base_path = fixture.root.path().join("bridge.toml");
    let text = format!("{BASE}\n[home]\ndir='{}'\n", fixture.path("").display());
    fs::write(&base_path, text).unwrap();
    let loaded = BridgeConfig::load_effective(&base_path).unwrap();
    assert_eq!(loaded.overlay.legacy_tool_cap(), Some(4));
    assert_eq!(loaded.config, loaded.base);
    let mut reloaded =
        SettingsHost::open(loaded.base, loaded.overlay, fixture.snapshot().await.prompts, vec![]).unwrap();
    let echo_screen_server::SettingsReply::Snapshot(snapshot) = reloaded.transaction(
        echo_screen_server::SettingsOperation::Snapshot,
        LinkStatusV1::Unconfigured,
    ) else {
        unreachable!("V1 after reload")
    };
    assert_eq!(snapshot.settings.agent.max_tool_calls, 4);
    assert!(!snapshot.restart_pending);
    let response = fixture.client.get(url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap()["settings"]["agent"],
        json!({"daily_request_cap":"500"})
    );
    fixture.close().await;
}
