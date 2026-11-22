use std::{
    os::unix::fs::PermissionsExt as _,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use futures_util::FutureExt as _;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_tungstenite::{WebSocketStream, tungstenite::http::StatusCode};

use crate::{
    MuseLink, MuseService as _, ServiceError, ServiceState,
    api::{Connector, Lease, Network, RefreshError, UpgradeError},
    link::OsRandom,
    pairing::{Pairing, Tokens},
    tasks::Tasks,
    trust,
};

use super::{Fake, ManualClock, Pki, assert_catalogue, fake::Vm, write_pairing};

const SAVED_AT: u64 = 1_700_000_000;
const ROTATION_BOUNDARY: u64 = SAVED_AT + 10_800;

pub(super) fn start(fake: &Fake, pki: &Pki, clock: &ManualClock) -> MuseLink {
    let pairing = Pairing::open(&fake.pairing).unwrap();
    let connector = Network::new(trust::client_config(&pki.pem).unwrap()).unwrap();
    MuseLink::spawn(
        &tokio::runtime::Handle::current(),
        pairing,
        connector,
        Tasks::open(&fake.task_path(), clock.clone(), OsRandom::new()).unwrap(),
    )
}

fn age_pairing(fake: &Fake, already_prefixed: bool) -> Vec<u8> {
    let mut record: Value = serde_json::from_slice(&std::fs::read(&fake.pairing).unwrap()).unwrap();
    record["access_token_saved_at"] = json!(SAVED_AT);
    if already_prefixed {
        record["refresh_token"] = json!("hatch_refresh:synthetic-refresh");
    }
    write_pairing(&fake.pairing, &record);
    std::fs::read(&fake.pairing).unwrap()
}

fn fresh_lease(id: &str, token: &str) -> Value {
    json!([{"vm_ws_url": "wss://127.0.0.1:1", "vm_auth_token": token, "vm_id": id}])
}

async fn register(vm: &mut Vm, link: &MuseLink) {
    let stream = vm.read_control_request().await;
    vm.control_response(stream, 200).await;
    let registration = vm.read_register(stream).await;
    assert_catalogue(&registration);
    vm.register_ack(stream, &registration["id"], true).await;
    assert_eq!(link.wait_registered().await, Ok(()));
}

fn idle(fake: &Fake) {
    assert!(fake.api.accept().now_or_never().is_none());
    assert!(fake.vm_listener.accept().now_or_never().is_none());
}

#[tokio::test]
async fn t3_stale_vm_lease_refetched_once() {
    let pki = Pki::new();
    for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
        let fake = Fake::new(&pki, &pki, "127.0.0.1").await;
        let link = MuseLink::start(&fake.pairing, &fake.task_path(), &pki.pem).unwrap();
        fake.lookup().await;
        assert!(
            fake.upgrade("vm%201%26x", "synthetic-default", Some(status))
                .await
                .is_none()
        );
        assert_eq!(link.state(), ServiceState::Connecting);
        fake.leases("synthetic-access", fresh_lease("replacement", "synthetic-replacement"))
            .await;
        let mut vm = fake
            .upgrade("replacement", "synthetic-replacement", None)
            .await
            .unwrap();
        register(&mut vm, &link).await;
        vm.barrier().await;
        idle(&fake);
        link.stop().await.unwrap();
    }
}

#[tokio::test]
async fn t3_no_default_falls_back_and_empty_list_does_not_dial() {
    let pki = Pki::new();
    let fake = Fake::new(&pki, &pki, "127.0.0.1").await;
    let link = MuseLink::start(&fake.pairing, &fake.task_path(), &pki.pem).unwrap();
    fake.leases(
        "synthetic-access",
        json!([
            {"vm_ws_url": "http://127.0.0.1:1", "vm_auth_token": "synthetic-invalid", "vm_id": "invalid", "default": true},
            {"vm_ws_url": "wss://127.0.0.1:1", "vm_auth_token": "synthetic-first", "vm_id": "", "vm_name": "fallback"},
            {"vm_ws_url": "wss://127.0.0.1:1", "vm_auth_token": "synthetic-last", "vm_id": "last"}
        ]),
    )
    .await;
    let mut vm = fake.upgrade("fallback", "synthetic-first", None).await.unwrap();
    register(&mut vm, &link).await;
    link.stop().await.unwrap();

    let empty = Fake::new(&pki, &pki, "127.0.0.1").await;
    let link = MuseLink::start(&empty.pairing, &empty.task_path(), &pki.pem).unwrap();
    empty.leases("synthetic-access", json!([])).await;
    assert_eq!(link.wait_registered().await, Err(ServiceError::PeerUnavailable));
    assert_eq!(link.state(), ServiceState::Disconnected);
    idle(&empty);
    link.stop().await.unwrap();
}

#[tokio::test]
async fn t3_repeated_upgrade_rejection_stops_attempt() {
    let pki = Pki::new();
    let fake = Fake::new(&pki, &pki, "127.0.0.1").await;
    let link = MuseLink::start(&fake.pairing, &fake.task_path(), &pki.pem).unwrap();
    for (id, status) in [("first", StatusCode::UNAUTHORIZED), ("second", StatusCode::FORBIDDEN)] {
        fake.leases("synthetic-access", fresh_lease(id, "synthetic-lease"))
            .await;
        assert!(fake.upgrade(id, "synthetic-lease", Some(status)).await.is_none());
    }
    assert_eq!(link.wait_registered().await, Err(ServiceError::PeerUnavailable));
    assert_eq!(link.state(), ServiceState::Disconnected);
    idle(&fake);
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    tokio::time::resume();
    fake.leases("synthetic-access", fresh_lease("next", "synthetic-new"))
        .await;
    let mut vm = fake.upgrade("next", "synthetic-new", None).await.unwrap();
    register(&mut vm, &link).await;
    link.stop().await.unwrap();
}

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
    let mut vm = fake.vm().await;
    register(&mut vm, &link).await;
    clock.set(ROTATION_BOUNDARY - 1);
    vm.barrier().await;
    idle(&fake);
    assert_eq!(std::fs::read(&fake.pairing).unwrap(), stored);

    clock.set(ROTATION_BOUNDARY);
    fake.refresh(200, response).await;
    fake.leases("synthetic-new-access", fresh_lease("rotated", "synthetic-new-lease"))
        .await;
    vm.expect_closed().await;
    let mut vm = fake.upgrade("rotated", "synthetic-new-lease", None).await.unwrap();
    register(&mut vm, &link).await;
    idle(&fake);
    link.stop().await.unwrap();
    let reopened = Pairing::open(&fake.pairing).unwrap();
    assert_eq!(reopened.access_token(), "synthetic-new-access");
    assert_eq!(reopened.refresh_token(), "synthetic-new-refresh");
    assert_eq!(reopened.saved_at(), ROTATION_BOUNDARY);
    assert_eq!(
        std::fs::metadata(&fake.pairing).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    let mut original: Value = serde_json::from_slice(&stored).unwrap();
    original["access_token"] = json!("synthetic-new-access");
    original["refresh_token"] = json!("synthetic-new-refresh");
    original["access_token_saved_at"] = json!(ROTATION_BOUNDARY);
    let rotated: Value = serde_json::from_slice(&std::fs::read(&fake.pairing).unwrap()).unwrap();
    assert_eq!(rotated, original);
}

fn replacement() -> Value {
    json!({"access_token": "synthetic-new-access", "refresh_token": "synthetic-new-refresh"})
}

#[tokio::test]
async fn t3_refresh_failure_preserves_pair() {
    let pki = Pki::new();
    let fake = Fake::new(&pki, &pki, "127.0.0.1").await;
    let bytes = age_pairing(&fake, false);
    let link = start(&fake, &pki, &ManualClock::new(ROTATION_BOUNDARY));
    fake.refresh(200, json!({"access_token": "synthetic-incomplete"})).await;
    assert_eq!(link.wait_registered().await, Err(ServiceError::PeerUnavailable));
    assert_eq!(link.state(), ServiceState::Disconnected);
    assert_eq!(std::fs::read(&fake.pairing).unwrap(), bytes);
    idle(&fake);
    link.stop().await.unwrap();
}

#[tokio::test]
async fn t3_revoked_refresh_removes_pair() {
    let pki = Pki::new();
    let fake = Fake::new(&pki, &pki, "127.0.0.1").await;
    age_pairing(&fake, false);
    let link = start(&fake, &pki, &ManualClock::new(ROTATION_BOUNDARY));
    fake.refresh(401, json!({})).await;
    assert_eq!(link.wait_registered().await, Err(ServiceError::NotReady));
    assert_eq!(link.state(), ServiceState::PairingRemoved);
    assert!(matches!(
        Pairing::open(&fake.pairing),
        Err(ServiceError::PairingUnavailable)
    ));
    idle(&fake);
    link.stop().await.unwrap();
    assert_eq!(link.state(), ServiceState::PairingRemoved);
}

#[derive(Default)]
struct ControlledRefresh {
    entered: Notify,
    release: Notify,
    calls: AtomicUsize,
}

impl Connector for Arc<ControlledRefresh> {
    type Socket = WebSocketStream<tokio::io::DuplexStream>;

    async fn lookup(&self, _: &Pairing) -> Result<Lease, ServiceError> {
        panic!("stop should prevent another lookup after rotation")
    }

    async fn upgrade(&self, _: &Pairing, _: &Lease) -> Result<Self::Socket, UpgradeError> {
        panic!("rotation should precede any upgrade")
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
    write_pairing(&path, &super::pairing_record());
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
    let reopened = Pairing::open(&path).unwrap();
    assert_eq!(reopened.access_token(), "synthetic-new-access");
    assert_eq!(reopened.refresh_token(), "synthetic-new-refresh");
    assert_eq!(reopened.saved_at(), ROTATION_BOUNDARY);
}
