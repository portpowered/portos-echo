use super::*;
use serde_json::{Value, json};

#[tokio::test]
async fn t3_settings_endpoint_patch_persists_only_valid_paired_edits() {
    let fixture = Fixture::open().await;
    let overlay_file = fixture.path("settings.toml");
    let url = fixture.url("/settings/v2");
    let before = snapshot_v2(&fixture, &url).await;
    assert_eq!(before["portos"]["endpoint_name"], "Echo Show");
    assert_eq!(before["portos"]["endpoint_enabled"], false);
    let patch = |name: &str| json!({"version": 2, "settings": {"portos": {"endpoint_name": name}}});
    assert_empty_refusal(
        fixture
            .client
            .patch(url.split('?').next().unwrap())
            .json(&patch("Hall Echo")),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    assert_empty_refusal(
        fixture
            .client
            .patch(&url)
            .header("Origin", "http://127.0.0.1:1")
            .json(&patch("Hall Echo")),
        StatusCode::FORBIDDEN,
    )
    .await;
    for name in [" \u{2003}".into(), "Hall\tEcho".into(), "界".repeat(65)] {
        let response = fixture.client.patch(&url).json(&patch(&name)).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({
                "version": 2,
                "message": "portos_endpoint.device_name: expected 1 to 64 characters, nonblank and without controls"
            })
        );
    }
    assert!(!overlay_file.exists());
    assert_eq!(snapshot_v2(&fixture, &url).await, before);

    let name = "界".repeat(64);
    let response = fixture.client.patch(&url).json(&patch(&name)).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"version": 2, "restart_pending": false})
    );
    let persisted = fs::read_to_string(overlay_file).unwrap();
    assert_eq!(persisted, format!("[portos_endpoint]\ndevice_name = \"{name}\"\n"));
    let mut expected = before;
    expected["portos"]["endpoint_name"] = json!(name);
    assert_eq!(snapshot_v2(&fixture, &url).await, expected);
    fixture.close().await;
}
