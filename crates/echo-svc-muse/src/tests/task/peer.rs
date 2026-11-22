use echo_contracts::{ids::TaskId, task::TaskEvent};
use serde_json::{Value, json};

use crate::{
    MuseLink, control,
    wire::{Body, Frame, Request, Response},
};

use super::{Fake, Vm};

pub(super) struct Chat {
    pub(super) stream: i64,
    pub(super) session: String,
}

impl Chat {
    pub(super) async fn emit(&self, vm: &mut Vm, seq: u64, name: &str, payload: Value) {
        vm.chunk(self.stream, event(&self.session, seq, name, payload), false)
            .await;
    }
}

pub(super) async fn connect(fake: &Fake, link: &MuseLink) -> (Vm, i64) {
    fake.lookup().await;
    let mut vm = fake.vm().await;
    let control = vm.read_control_request().await;
    let register = vm.read_register(control).await;
    vm.control_response(control, 200).await;
    vm.register_ack(control, &register["id"], true).await;
    assert_eq!(link.wait_registered().await, Ok(()));
    (vm, control)
}

pub(super) async fn send_request(vm: &mut Vm) -> (i64, Value) {
    let (id, request) = request(vm, "/chat/stream").await;
    let json: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(json["device_id"], "gadget-fixture");
    assert_eq!(json["output_modality"], "text");
    (id, json)
}

pub(super) async fn send_ack(vm: &mut Vm, id: i64, session: &str) {
    vm.response(
        id,
        200,
        json!({"result":{"session_id":session,"is_thread":true,"message_id":"user-1"}}),
        true,
    )
    .await;
}

pub(super) async fn subscribe(vm: &mut Vm, session: &str) -> Chat {
    let (stream, request) = request(vm, "/chat/subscribe").await;
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body, json!({"session_id":session}));
    assert!(
        request
            .headers
            .iter()
            .any(|header| header.key == "Accept" && header.value == "application/x-ndjson")
    );
    vm.control_response(stream, 200).await;
    Chat {
        stream,
        session: session.into(),
    }
}

async fn request(vm: &mut Vm, route: &str) -> (i64, Request) {
    let frame = vm.next_frame().await;
    let Body::Request(mut request) = frame.body else {
        panic!("expected {route}");
    };
    assert_eq!(request.verb, "POST");
    assert_eq!(request.path, route);
    assert!(
        request
            .headers
            .iter()
            .any(|header| header.key == "x-request-id" && header.value.len() == 36)
    );
    assert!(
        request
            .headers
            .iter()
            .any(|header| header.key == "x-app-id" && header.value == "musegadget")
    );
    while !request.end_body {
        let next = vm.next_frame().await;
        assert_eq!(next.id, frame.id);
        let Body::Chunk { data, end_body } = next.body else {
            panic!("expected request body chunk");
        };
        request.body.extend(data);
        request.end_body = end_body;
    }
    (frame.id, request)
}

pub(super) async fn invoke(
    vm: &mut Vm,
    control_id: i64,
    call: &str,
    id: &TaskId,
    status: &str,
    summary: &str,
) -> Value {
    let invoke = json!({
        "id":call, "method":"link.invoke", "command":"echo.report.result", "timeout_ms":10000,
        "params":{"task_id":id,"status":status,"summary":summary},
    });
    vm.chunk(control_id, control::encode(&invoke).unwrap(), false).await;
    vm.read_register(control_id).await
}

pub(super) async fn expect_reset(vm: &mut Vm, id: i64) {
    let frame = vm.next_frame().await;
    assert_eq!(frame.id, id);
    assert_eq!(
        frame.body,
        Body::Reset {
            code: 1,
            reason: "request ended".into()
        }
    );
}

pub(super) fn event(session: &str, seq: u64, name: &str, mut payload: Value) -> Vec<u8> {
    payload["session_id"] = json!(session);
    let mut bytes = serde_json::to_vec(&json!({"type":"event","seq":seq,"event":name,"payload":payload})).unwrap();
    bytes.push(b'\n');
    bytes
}

pub(super) fn progress(id: &TaskId, message: &str) -> TaskEvent {
    TaskEvent::Progress {
        task_id: id.clone(),
        message: message.into(),
    }
}

impl Vm {
    pub(super) async fn response(&mut self, id: i64, status: i32, body: Value, end_body: bool) {
        self.send(&Frame {
            id,
            body: Body::Response(Response {
                status,
                body: serde_json::to_vec(&body).unwrap(),
                end_body,
                ..Response::default()
            }),
        })
        .await;
    }

    pub(super) async fn chunk(&mut self, id: i64, data: Vec<u8>, end_body: bool) {
        self.send(&Frame {
            id,
            body: Body::Chunk { data, end_body },
        })
        .await;
    }

    pub(super) async fn reset(&mut self, id: i64) {
        self.send(&Frame {
            id,
            body: Body::Reset {
                code: 1,
                reason: "synthetic reset".into(),
            },
        })
        .await;
    }
}
