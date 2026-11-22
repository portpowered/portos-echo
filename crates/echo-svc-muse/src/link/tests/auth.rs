use std::{
    future::ready,
    os::unix::fs::PermissionsExt as _,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use futures_util::FutureExt as _;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_tungstenite::WebSocketStream;

use crate::{
    MuseLink, MuseService as _, ServiceError, ServiceState,
    api::{Connector, Lease, RefreshError, UpgradeError},
    link::OsRandom,
    pairing::{Pairing, Tokens},
    support::{Fake, Pki, SAVED_AT, age_pairing, fresh_lease, pairing_record, register, write_pairing},
    tasks::Tasks,
};

use super::{ManualClock, start};

const ROTATION_BOUNDARY: u64 = SAVED_AT + 10_800;

#[tokio::test]
async fn t3_rotates_both_tokens_at_three_hours() {
    let pki = Pki::new();
    rotation_journey(&pki, true, json!({"payload": replacement()})).await;
    rotation_journey(&pki, false, replacement()).await;
}

async fn rotation_journey(pki: &Pki, prefixed: bool, response: Value) {
    let fake = Fake::new(pki, pki, "127.0.0.1").await;
    let stored = age_pairing(&fake, prefixed);
    let clock = ManualClock::new(ROTATION_BOUNDARY - 2);
    let link = start(&fake, pki, &clock);
    fake.lookup().await;
    let (mut vm, _) = register(fake.vm().await, &link).await;
    clock.set(ROTATION_BOUNDARY - 1);
    vm.barrier().await;
    fake.assert_idle();
    assert_eq!(std::fs::read(&fake.pairing).unwrap(), stored);

    clock.set(ROTATION_BOUNDARY);
    fake.refresh(200, response).await;
    fake.leases("synthetic-new-access", fresh_lease("rotated", "synthetic-new-lease"))
        .await;
    vm.expect_closed().await;
    let vm = fake.upgrade("rotated", "synthetic-new-lease", None).await.unwrap();
    let _ = register(vm, &link).await;
    fake.assert_idle();
    link.stop().await.unwrap();
    assert_saved_rotation(&fake, &stored);
}

fn assert_saved_rotation(fake: &Fake, stored: &[u8]) {
    let reopened = Pairing::open(&fake.pairing).unwrap();
    assert_eq!(reopened.access_token(), "synthetic-new-access");
    assert_eq!(reopened.refresh_token(), "synthetic-new-refresh");
    assert_eq!(reopened.saved_at(), ROTATION_BOUNDARY);
    assert_eq!(
        std::fs::metadata(&fake.pairing).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    let mut original: Value = serde_json::from_slice(stored).unwrap();
    original["access_token"] = json!("synthetic-new-access");
    original["refresh_token"] = json!("synthetic-new-refresh");
    original["access_token_saved_at"] = json!(ROTATION_BOUNDARY);
    let rotated: Value = serde_json::from_slice(&std::fs::read(&fake.pairing).unwrap()).unwrap();
    assert_eq!(rotated, original);
}

fn replacement() -> Value {
    json!({"access_token": "synthetic-new-access", "refresh_token": "synthetic-new-refresh"})
}

#[derive(Default)]
struct ControlledRefresh {
    entered: Notify,
    release: Notify,
    calls: AtomicUsize,
    lookups: AtomicUsize,
    upgrades: AtomicUsize,
}

impl Connector for Arc<ControlledRefresh> {
    type Socket = WebSocketStream<tokio::io::DuplexStream>;

    fn lookup(&self, _: &Pairing) -> impl Future<Output = Result<Lease, ServiceError>> + Send {
        self.lookups.fetch_add(1, Ordering::Relaxed);
        ready(Err(ServiceError::PeerUnavailable))
    }

    fn upgrade(&self, _: &Pairing, _: &Lease) -> impl Future<Output = Result<Self::Socket, UpgradeError>> + Send {
        self.upgrades.fetch_add(1, Ordering::Relaxed);
        ready(Err(UpgradeError::Unavailable))
    }

    async fn refresh(&self, _: &Pairing) -> Result<Tokens, RefreshError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.entered.notify_one();
        self.release.notified().await;
        Ok(serde_json::from_value(replacement()).unwrap())
    }
}

#[tokio::test]
async fn t1_auth_single_refresh_owner_commits_before_stop_releases_pairing() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pairing.json");
    write_pairing(&path, &pairing_record());
    let connector = Arc::new(ControlledRefresh::default());
    let link = MuseLink::spawn(
        &tokio::runtime::Handle::current(),
        Pairing::open(&path).unwrap(),
        connector.clone(),
        Tasks::open(
            &root.path().join("tasks.json"),
            ManualClock::new(ROTATION_BOUNDARY),
            OsRandom::new(),
        )
        .unwrap(),
    );
    connector.entered.notified().await;
    let mut stop = std::pin::pin!(link.stop());
    assert!(stop.as_mut().now_or_never().is_none());
    assert!(matches!(Pairing::open(&path), Err(ServiceError::UnsafePairing)));
    connector.release.notify_one();
    stop.await.unwrap();
    assert_eq!(link.state(), ServiceState::Stopped);
    assert_eq!(connector.calls.load(Ordering::Relaxed), 1);
    assert_eq!(connector.lookups.load(Ordering::Relaxed), 0);
    assert_eq!(connector.upgrades.load(Ordering::Relaxed), 0);
    let reopened = Pairing::open(&path).unwrap();
    assert_eq!(reopened.access_token(), "synthetic-new-access");
    assert_eq!(reopened.refresh_token(), "synthetic-new-refresh");
    assert_eq!(reopened.saved_at(), ROTATION_BOUNDARY);
}
