mod auth;
mod fake;
mod task;

use std::{os::unix::fs::PermissionsExt, path::Path};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use echo_agent_api::Task;
use echo_contracts::ids::DelegationId;
use futures_util::FutureExt as _;
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::{
    MuseLink, MuseService as _, ServiceError, ServiceState,
    clock::{Clock, Wait},
};

use self::fake::{Fake, Pki};

#[derive(Clone)]
pub(super) struct ManualClock(watch::Sender<u64>);

impl ManualClock {
    pub(super) fn new(now: u64) -> Self {
        Self(watch::Sender::new(now))
    }

    pub(super) fn set(&self, now: u64) {
        self.0.send_replace(now);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> u64 {
        *self.0.borrow()
    }

    fn wait_until(&self, deadline: u64) -> Wait<'_> {
        Box::pin(async move {
            let _ = self.0.subscribe().wait_for(|now| *now >= deadline).await;
        })
    }
}

pub(super) fn pairing_record() -> Value {
    json!({
        "version": 1,
        "node_id": "gadget-fixture",
        "access_token": "synthetic-access",
        "refresh_token": "synthetic-refresh",
        "access_token_saved_at": 1700000000,
        "noise_static_key": STANDARD.encode([7; 32]),
    })
}

pub(super) fn write_pairing(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

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

fn assert_catalogue(register: &Value) {
    assert_eq!(register["type"], "req");
    assert_eq!(register["method"], "link.register");
    let params = &register["params"];
    assert_eq!(params["node_id"], "gadget-fixture");
    assert_eq!(params["device_family"], "gadget");
    assert_eq!(params["platform"], "linux");
    assert_eq!(params["model_id"], "portos-echo");
    assert_eq!(params["is_wakeup_supported"], false);
    let catalogue = params["commands_v2"].as_object().unwrap();
    assert_eq!(catalogue.keys().collect::<Vec<_>>(), ["echo.report.result"]);
    let report = &catalogue["echo.report.result"];
    assert_eq!(report["timeout_ms"], 15000);
    assert_eq!(report["optional"], json!({}));
    let fields = report["required"].as_object().unwrap();
    assert_eq!(fields.keys().collect::<Vec<_>>(), ["status", "summary", "task_id"]);
    for field in fields.values() {
        assert_eq!(field["type"], "string");
    }
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

        let fake = Fake::with_vm_name(&trusted, issuer, name).await;
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

pub(super) fn demo_task() -> Task {
    Task {
        delegation_id: DelegationId::new("synthetic-delegation").unwrap(),
        instructions: "Read the fixture".into(),
        context: json!({}),
        deadline_seconds: 2,
        max_tool_calls: 1,
    }
}
