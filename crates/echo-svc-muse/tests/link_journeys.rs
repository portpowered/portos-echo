//! T3 component journeys through the shipped Muse service, with loopback peers.
#![cfg(target_os = "linux")]

#[path = "journeys/auth.rs"]
mod auth;
#[path = "../src/noise/mod.rs"]
mod noise;
mod support;
#[path = "journeys/task.rs"]
mod task;
#[path = "../src/wire/mod.rs"]
mod wire;

use std::os::unix::fs::PermissionsExt as _;

use echo_svc_muse::{MuseLink, MuseService, ServiceError, ServiceState};
use futures_util::FutureExt as _;
use serde_json::json;

use support::{Fake, Pki, assert_catalogue, demo_task};

#[tokio::test]
async fn t3_registers_only_echo_report_result() {
    let pki = Pki::new();
    let fake = Fake::new(&pki, &pki, "127.0.0.1").await;
    let link = MuseLink::start(&fake.pairing, &fake.task_path(), &pki.pem).unwrap();
    fake.lookup().await;
    let mut vm = fake.vm().await;
    let stream = vm.read_control_request().await;
    vm.control_response(stream, 200).await;
    let register = vm.read_register(stream).await;
    assert_catalogue(&register);
    assert_eq!(link.state(), ServiceState::Connecting);

    vm.register_ack(stream, &json!("unrelated"), true).await;
    vm.barrier().await;
    assert_eq!(link.state(), ServiceState::Connecting);
    vm.register_ack(stream, &register["id"], true).await;
    assert_eq!(link.wait_registered().await, Ok(()));
    assert_eq!(link.state(), ServiceState::Registered);
    assert_eq!(link.stop().await, Ok(()));
    assert_eq!(link.state(), ServiceState::Stopped);
    vm.expect_closed().await;
    assert!(fake.pairing.exists());
}

#[tokio::test]
async fn t3_refuses_unsafe_pairing_before_io() {
    let pki = Pki::new();
    let fake = Fake::new(&pki, &pki, "127.0.0.1").await;
    std::fs::set_permissions(&fake.pairing, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        MuseLink::start(&fake.pairing, &fake.task_path(), &pki.pem),
        Err(ServiceError::UnsafePairing)
    ));
    assert!(fake.api.accept().now_or_never().is_none());
    assert!(fake.vm_listener.accept().now_or_never().is_none());
}

#[tokio::test]
async fn t3_tls_rejects_untrusted_peer_and_wrong_name() {
    let trusted = Pki::new();
    let other = Pki::new();
    for (issuer, name) in [(&other, "127.0.0.1"), (&trusted, "wrong.example.invalid")] {
        let fake = Fake::new(issuer, &trusted, name).await;
        let link = MuseLink::start(&fake.pairing, &fake.task_path(), &trusted.pem).unwrap();
        assert!(fake.accept_api().await.is_err());
        assert_eq!(link.wait_registered().await, Err(ServiceError::PeerUnavailable));
        assert_eq!(link.state(), ServiceState::Disconnected);
        assert!(fake.vm_listener.accept().now_or_never().is_none());

        let fake = Fake::with_names(&trusted, issuer, "127.0.0.1", name).await;
        let link = MuseLink::start(&fake.pairing, &fake.task_path(), &trusted.pem).unwrap();
        fake.lookup().await;
        assert!(fake.accept_vm().await.is_err());
        assert_eq!(link.wait_registered().await, Err(ServiceError::PeerUnavailable));
        assert_eq!(link.state(), ServiceState::Disconnected);
    }
}

#[tokio::test]
async fn t3_registration_refusal_never_ready() {
    let pki = Pki::new();
    for status in [403, 200] {
        let fake = Fake::new(&pki, &pki, "127.0.0.1").await;
        let link = MuseLink::start(&fake.pairing, &fake.task_path(), &pki.pem).unwrap();
        fake.lookup().await;
        let mut vm = fake.vm().await;
        let stream = vm.read_control_request().await;
        let register = vm.read_register(stream).await;
        vm.control_response(stream, status).await;
        if status == 200 {
            vm.register_ack(stream, &register["id"], false).await;
        }
        assert_eq!(link.wait_registered().await, Err(ServiceError::NotReady));
        assert_eq!(link.state(), ServiceState::Disconnected);
        assert_eq!(link.submit(demo_task()).await, Err(ServiceError::NotReady));
        vm.expect_closed().await;
    }
}

#[tokio::test]
async fn t3_final_control_chunk_unpairs_without_reconnect() {
    let pki = Pki::new();
    for event in ["link.unpaired", "node.unpaired"] {
        let fake = Fake::new(&pki, &pki, "127.0.0.1").await;
        let link = MuseLink::start(&fake.pairing, &fake.task_path(), &pki.pem).unwrap();
        let (mut vm, stream) = support::connect(&fake, &link).await;
        let bytes = support::encode_control(&json!({"type":"evt","event":event}));
        let split = bytes.len() / 2;
        vm.chunk(stream, bytes[..split].to_vec(), false).await;
        vm.chunk(stream, bytes[split..].to_vec(), true).await;
        assert_unpaired(&fake, &link, &mut vm).await;
    }
}

#[tokio::test]
async fn t3_final_control_response_unpairs_before_registration() {
    let pki = Pki::new();
    for event in ["link.unpaired", "node.unpaired"] {
        let fake = Fake::new(&pki, &pki, "127.0.0.1").await;
        let link = MuseLink::start(&fake.pairing, &fake.task_path(), &pki.pem).unwrap();
        fake.lookup().await;
        let mut vm = fake.vm().await;
        let stream = vm.read_control_request().await;
        let _ = vm.read_register(stream).await;
        let data = support::encode_control(&json!({"type":"evt","event":event}));
        vm.send(&wire::Frame {
            id: stream,
            body: wire::Body::Response(wire::Response {
                status: 200,
                body: data,
                end_body: true,
                ..wire::Response::default()
            }),
        })
        .await;
        assert_unpaired(&fake, &link, &mut vm).await;
    }
}

async fn assert_unpaired(fake: &Fake, link: &MuseLink, vm: &mut support::Vm) {
    vm.expect_closed().await;
    assert_eq!(link.state(), ServiceState::PairingRemoved);
    assert!(!fake.pairing.exists());
    assert_eq!(link.submit(demo_task()).await, Err(ServiceError::NotReady));
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    tokio::time::resume();
    assert_eq!(link.state(), ServiceState::PairingRemoved);
    fake.assert_idle();
    assert_eq!(link.stop().await, Ok(()));
}
