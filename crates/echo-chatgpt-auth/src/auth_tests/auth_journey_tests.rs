//! T3 public-library journey: real HTTP/storage and explicitly controlled clock.
use super::auth_support::*;
use crate::{AuthError, auth_clock::Clock, auth_store};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

struct Wall(AtomicU64);
impl Clock for Wall {
    fn unix(&self) -> Result<u64, AuthError> {
        Ok(self.0.load(Ordering::SeqCst))
    }
    fn monotonic(&self) -> Duration {
        Duration::ZERO
    }
    fn wait(&self, _: Duration) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::task::yield_now())
    }
}
fn rotation() -> (serde_json::Value, String) {
    let mut rotated: serde_json::Value = serde_json::from_str(&tokens(Some("synthetic-account"))).unwrap();
    rotated["refresh_token"] = "synthetic-rotated".into();
    let old_access = token(Some("synthetic-account"));
    let mut claims: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(old_access.split('.').nth(1).unwrap()).unwrap()).unwrap();
    claims["exp"] = (NOW + 7200).into();
    let new_access = format!("e30.{}.synthetic", URL_SAFE_NO_PAD.encode(claims.to_string()));
    rotated["access_token"] = new_access.clone().into();
    (rotated, new_access)
}
#[tokio::test]
async fn t3_journey_device_reload_fresh_due_refresh_and_final_reload() {
    let (rotated, new_access) = rotation();
    let mock = Mock::start(vec![
        device(),
        (403, "{}".into()),
        (404, "{}".into()),
        approval(),
        (200, tokens(Some("synthetic-account"))),
        (200, rotated.to_string()),
    ])
    .await;
    let directory = directory();
    let source = mock.source(&directory);
    let mut callbacks = 0;
    let initial = source
        .device_sign_in(|prompt| {
            callbacks += 1;
            assert_eq!(prompt.user_code, "TEST-CODE");
            assert_eq!(mock.requests.lock().unwrap().len(), 1);
        })
        .await
        .unwrap();
    assert_eq!(callbacks, 1);
    assert_eq!(initial.access_token.expose(), &token(Some("synthetic-account")));
    let wall = Arc::new(Wall(AtomicU64::new(NOW)));
    let mut reloaded = mock.source(&directory);
    reloaded.clock = wall.clone();
    assert_eq!(
        reloaded.current().await.unwrap().access_token.expose(),
        initial.access_token.expose()
    );
    assert_eq!(mock.requests.lock().unwrap().len(), 5);
    wall.0.store(NOW + 3300, Ordering::SeqCst);
    let fresh = reloaded.current().await.unwrap();
    assert_eq!(fresh.access_token.expose(), &new_access);
    assert_eq!(fresh.account_id, "synthetic-account");
    let disk = auth_store::load(&source.file).unwrap();
    assert_eq!(disk.access.expose(), fresh.access_token.expose());
    assert_eq!(disk.refresh.expose(), "synthetic-rotated");
    assert_eq!(disk.last_refresh, NOW + 3300);
    let bytes = std::fs::read(&source.file).unwrap();
    let mut final_source = mock.source(&directory);
    final_source.clock = wall;
    assert_eq!(final_source.current().await.unwrap().access_token.expose(), &new_access);
    assert_eq!(std::fs::read(&source.file).unwrap(), bytes);
    assert_eq!(
        mock.requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.path.as_str())
            .collect::<Vec<_>>(),
        ["/user", "/poll", "/poll", "/poll", "/token", "/token"]
    );
    mock.stop().await;
}
