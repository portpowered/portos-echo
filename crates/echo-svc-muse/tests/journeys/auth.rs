use echo_svc_muse::{MuseLink, MuseService as _, ServiceError, ServiceState};
use serde_json::json;
use tokio_tungstenite::tungstenite::http::StatusCode;

use crate::support::{Fake, Pki, Vm, age_pairing, assert_catalogue, fresh_lease};

async fn acknowledge(vm: &mut Vm, link: &MuseLink) {
    let stream = vm.read_control_request().await;
    vm.control_response(stream, 200).await;
    let registration = vm.read_register(stream).await;
    assert_catalogue(&registration);
    vm.register_ack(stream, &registration["id"], true).await;
    assert_eq!(link.wait_registered().await, Ok(()));
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
        acknowledge(&mut vm, &link).await;
        vm.barrier().await;
        fake.assert_idle();
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
    acknowledge(&mut vm, &link).await;
    link.stop().await.unwrap();

    let empty = Fake::new(&pki, &pki, "127.0.0.1").await;
    let link = MuseLink::start(&empty.pairing, &empty.task_path(), &pki.pem).unwrap();
    empty.leases("synthetic-access", json!([])).await;
    assert_eq!(link.wait_registered().await, Err(ServiceError::PeerUnavailable));
    assert_eq!(link.state(), ServiceState::Disconnected);
    empty.assert_idle();
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
    fake.assert_idle();
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    tokio::time::resume();
    fake.leases("synthetic-access", fresh_lease("next", "synthetic-new"))
        .await;
    let mut vm = fake.upgrade("next", "synthetic-new", None).await.unwrap();
    acknowledge(&mut vm, &link).await;
    link.stop().await.unwrap();
}

#[tokio::test]
async fn t3_refresh_failure_preserves_pair() {
    let pki = Pki::new();
    let fake = Fake::new(&pki, &pki, "127.0.0.1").await;
    let bytes = age_pairing(&fake, false);
    let link = MuseLink::start(&fake.pairing, &fake.task_path(), &pki.pem).unwrap();
    fake.refresh(200, json!({"access_token": "synthetic-incomplete"})).await;
    assert_eq!(link.wait_registered().await, Err(ServiceError::PeerUnavailable));
    assert_eq!(link.state(), ServiceState::Disconnected);
    assert_eq!(std::fs::read(&fake.pairing).unwrap(), bytes);
    fake.assert_idle();
    link.stop().await.unwrap();
}

#[tokio::test]
async fn t3_revoked_refresh_removes_pair() {
    let pki = Pki::new();
    let fake = Fake::new(&pki, &pki, "127.0.0.1").await;
    age_pairing(&fake, false);
    let link = MuseLink::start(&fake.pairing, &fake.task_path(), &pki.pem).unwrap();
    fake.refresh(401, json!({})).await;
    assert_eq!(link.wait_registered().await, Err(ServiceError::NotReady));
    assert_eq!(link.state(), ServiceState::PairingRemoved);
    assert!(!fake.pairing.exists());
    fake.assert_idle();
    link.stop().await.unwrap();
    assert_eq!(link.state(), ServiceState::PairingRemoved);
}
