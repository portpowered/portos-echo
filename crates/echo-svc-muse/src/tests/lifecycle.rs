use echo_contracts::{ids::TaskId, task::TaskEvent};
use futures_util::FutureExt as _;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::{
    MuseLink, MuseService as _, ServiceError, ServiceState,
    control::{Handler, HeldInvoke},
    pairing::Pairing,
};

use super::{
    Fake, ManualClock, Pki, auth, demo_task,
    fake::Vm,
    task::{NOW, peer, start_chat},
    wait_state,
};

const OUTPUT_LIMIT: usize = 96 * 1024;
const CONTROL_LIMIT: usize = 256 * 1024;

struct Fixture {
    fake: Fake,
    clock: ManualClock,
    link: MuseLink,
    vm: Vm,
    control: i64,
    held: mpsc::Receiver<HeldInvoke>,
}

impl Fixture {
    async fn open() -> Self {
        let pki = Pki::new();
        let fake = Fake::new(&pki, &pki, "127.0.0.1").await;
        let clock = ManualClock::new(NOW);
        let (send, held) = mpsc::channel(8);
        let link = auth::start_with(&fake, &pki, &clock, Handler::holding(send));
        fake.lookup().await;
        let (vm, control) = peer::register(fake.vm().await, &link).await;
        Self {
            fake,
            clock,
            link,
            vm,
            control,
            held,
        }
    }

    async fn hold(&mut self, id: &str, task: &TaskId) -> HeldInvoke {
        self.vm.send_control(self.control, &invoke(id, task)).await;
        let call = self.held.recv().await.unwrap();
        assert_eq!(call.id, id);
        call
    }
}

fn invoke(id: &str, task: &TaskId) -> Value {
    json!({
        "id":id, "method":"link.invoke", "command":"echo.report.result",
        "params":{"task_id":task,"status":"completed","summary":"Fixture read"},
    })
}

#[tokio::test]
async fn t3_four_invokes_run_and_fifth_waits() {
    let mut fixture = Fixture::open().await;
    let task = TaskId::new("task-fixture").unwrap();
    for id in ["one", "two", "three", "four", "five"] {
        fixture.vm.send_control(fixture.control, &invoke(id, &task)).await;
    }
    let mut active = Vec::new();
    for expected in ["one", "two", "three", "four"] {
        let call = fixture.held.recv().await.unwrap();
        assert_eq!(call.id, expected);
        active.push(call);
    }
    fixture.vm.barrier().await;
    assert!(fixture.held.recv().now_or_never().is_none());

    release_and_expect(&mut fixture, active.remove(0)).await;
    let fifth = fixture.held.recv().await.unwrap();
    assert_eq!(fifth.id, "five");
    active.push(fifth);
    assert_eq!(active.iter().filter(|call| !call.finish.is_closed()).count(), 4);
    for call in active {
        release_and_expect(&mut fixture, call).await;
    }
}

async fn release_and_expect(fixture: &mut Fixture, call: HeldInvoke) {
    call.finish.send(Some("finished".into())).unwrap();
    assert_eq!(
        fixture.vm.read_control(fixture.control).await.0,
        json!({"method":"link.result","id":call.id,"ok":true,"payload":{"output":"finished","truncated":false}})
    );
}

#[tokio::test]
async fn t3_invoke_output_is_clipped_to_96_kib() {
    let mut fixture = Fixture::open().await;
    let task = TaskId::new("task-fixture").unwrap();
    let escaped = "\u{1}é";
    let samples = [
        ("ascii", "a".repeat(OUTPUT_LIMIT + 1), "a".repeat(OUTPUT_LIMIT)),
        (
            "escaped",
            format!("a{}", escaped.repeat(OUTPUT_LIMIT / 8)),
            format!("a{}\u{1}", escaped.repeat(OUTPUT_LIMIT / 8 - 1)),
        ),
    ];
    for (id, sample, prefix) in samples {
        fixture.hold(id, &task).await.finish.send(Some(sample)).unwrap();
        let (result, size) = fixture.vm.read_control(fixture.control).await;
        assert_eq!(result["id"], id);
        assert_eq!(result["ok"], true);
        assert_eq!(result["payload"]["truncated"], true);
        assert!(result["payload"]["output"].as_str() == Some(prefix.as_str()));
        assert!(result["payload"]["output"].to_string().len() - 2 <= OUTPUT_LIMIT);
        assert!(size <= CONTROL_LIMIT);
    }

    let id = "b".repeat(CONTROL_LIMIT - 1024);
    fixture
        .hold(&id, &task)
        .await
        .finish
        .send(Some("a".repeat(OUTPUT_LIMIT)))
        .unwrap();
    let (result, size) = fixture.vm.read_control(fixture.control).await;
    assert!(result["id"].as_str() == Some(id.as_str()));
    assert_eq!(result["payload"]["truncated"], true);
    assert_eq!(size, CONTROL_LIMIT - 1);
}

#[tokio::test]
async fn t3_builtins_refused_without_effect() {
    let mut fixture = Fixture::open().await;
    let (task, chat) = start_chat(&fixture.fake, &fixture.link, &mut fixture.vm, demo_task()).await;
    let legitimate = fixture.hold("callback", &task).await;
    for command in ["system.run", "file.read", "file.write", "device.health"] {
        let mut blocked = invoke(command, &task);
        blocked["command"] = json!(command);
        fixture.vm.send_control(fixture.control, &blocked).await;
        assert_eq!(
            fixture.vm.read_control(fixture.control).await.0,
            json!({"method":"link.result","id":command,"ok":false,"error":"unknown command"})
        );
    }
    assert!(fixture.held.recv().now_or_never().is_none());
    assert!(fixture.link.next_event(&task).now_or_never().is_none());
    legitimate.finish.send(None).unwrap();
    assert_eq!(
        fixture.vm.read_control(fixture.control).await.0,
        json!({"method":"link.result","id":"callback","ok":true,"payload":{"accepted":true,"task_id":task}})
    );
    peer::expect_reset(&mut fixture.vm, chat.stream).await;
    assert_eq!(
        fixture.link.next_event(&task).await,
        Ok(Some(TaskEvent::Completed {
            task_id: task,
            summary: "Fixture read".into()
        }))
    );
}

#[tokio::test]
async fn t3_unpair_stops_and_removes_pairing() {
    for event in ["link.unpaired", "node.unpaired"] {
        let mut fixture = Fixture::open().await;
        let (task, _) = start_chat(&fixture.fake, &fixture.link, &mut fixture.vm, demo_task()).await;
        let mut held = Vec::new();
        for id in ["one", "two", "three", "four"] {
            held.push(fixture.hold(id, &task).await);
        }
        fixture
            .vm
            .send_control(fixture.control, &invoke("waiting", &task))
            .await;
        fixture.vm.barrier().await;
        assert!(fixture.held.recv().now_or_never().is_none());
        fixture
            .vm
            .send_control(fixture.control, &json!({"type":"evt","event":event}))
            .await;
        wait_state(&fixture.link, ServiceState::PairingRemoved).await;
        fixture.vm.expect_closed().await;
        assert!(held.iter().all(|call| call.finish.is_closed()));
        assert!(fixture.held.recv().await.is_none());
        assert!(matches!(
            Pairing::open(&fixture.fake.pairing),
            Err(ServiceError::PairingUnavailable)
        ));
        assert_eq!(fixture.link.submit(demo_task()).await, Err(ServiceError::NotReady));
        assert!(fixture.link.next_event(&task).now_or_never().is_none());
        fixture.clock.set(NOW + 2);
        assert_eq!(
            fixture.link.next_event(&task).await,
            Ok(Some(TaskEvent::Failed {
                task_id: task,
                retryable: false,
                message: "Muse did not report a result in time".into(),
            }))
        );
        assert_eq!(fixture.link.stop().await, Ok(()));
        assert_eq!(fixture.link.state(), ServiceState::PairingRemoved);
        auth::idle(&fixture.fake);
    }
}

#[tokio::test]
async fn t3_unpair_stops_when_pairing_removal_fails() {
    let mut fixture = Fixture::open().await;
    let held = fixture.hold("held", &TaskId::new("task-fixture").unwrap()).await;
    std::fs::rename(&fixture.fake.pairing, fixture.fake.pairing.with_extension("moved")).unwrap();
    std::fs::create_dir(&fixture.fake.pairing).unwrap();
    fixture
        .vm
        .send_control(fixture.control, &json!({"type":"evt","event":"link.unpaired"}))
        .await;
    wait_state(&fixture.link, ServiceState::PairingRemoved).await;
    fixture.vm.expect_closed().await;
    assert!(held.finish.is_closed());
    assert_eq!(fixture.link.stop().await, Err(ServiceError::Storage));
    assert_eq!(fixture.link.submit(demo_task()).await, Err(ServiceError::NotReady));
    auth::idle(&fixture.fake);
}

#[tokio::test]
async fn t3_stop_drains_without_removing_pairing() {
    let mut fixture = Fixture::open().await;
    let (task, _) = start_chat(&fixture.fake, &fixture.link, &mut fixture.vm, demo_task()).await;
    let held = fixture.hold("held", &task).await;
    let before = std::fs::read(&fixture.fake.pairing).unwrap();
    assert_eq!(fixture.link.stop().await, Ok(()));
    fixture.vm.expect_closed().await;
    assert!(held.finish.is_closed());
    assert!(fixture.held.recv().await.is_none());
    assert_eq!(fixture.link.state(), ServiceState::Stopped);
    assert_eq!(fixture.link.submit(demo_task()).await, Err(ServiceError::NotReady));
    assert!(fixture.link.next_event(&task).now_or_never().is_none());
    assert_eq!(std::fs::read(&fixture.fake.pairing).unwrap(), before);
    assert!(Pairing::open(&fixture.fake.pairing).is_ok());
    auth::idle(&fixture.fake);
}

#[tokio::test]
async fn t3_oversize_control_fails_before_dispatch() {
    let mut fixture = Fixture::open().await;
    let oversized = u32::try_from(CONTROL_LIMIT + 1).unwrap().to_le_bytes();
    fixture.vm.control_bytes(fixture.control, oversized.to_vec()).await;
    fixture.vm.expect_closed().await;
    wait_state(&fixture.link, ServiceState::Disconnected).await;
    assert!(fixture.held.recv().now_or_never().is_none());
    fixture.link.stop().await.unwrap();

    let mut fresh = Fixture::open().await;
    let task = TaskId::new("task-fixture").unwrap();
    let mut valid = invoke("valid", &task);
    valid["padding"] = json!("");
    let size = valid.to_string().len() + size_of::<u32>();
    valid["padding"] = json!("a".repeat(CONTROL_LIMIT - size));
    fresh.vm.send_control(fresh.control, &valid).await;
    let call = fresh.held.recv().await.unwrap();
    assert_eq!(call.id, "valid");
    release_and_expect(&mut fresh, call).await;
}
