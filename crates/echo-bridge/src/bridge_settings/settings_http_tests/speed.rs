use super::*;

/// A wire-only tier name and a speed that disagrees with the legacy boolean are both refused.
pub(super) async fn refuse_speed_patches(fixture: &Fixture, url: &str) {
    use serde_json::{Value, json};
    let unknown = json!({"version":2,"settings":{"codex":{"delegate_speed":"priority"}}});
    assert_eq!(
        fixture.client.patch(url).json(&unknown).send().await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    let conflict = json!({"version":2,"settings":{"codex":{"delegate_fast":false,"delegate_speed":"fast"}}});
    let response = fixture.client.patch(url).json(&conflict).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"version":2,"message":"codex.delegate_speed: conflicts with codex.delegate_fast"})
    );
}
