use echo_agent_api::Task;
use echo_contracts::{ids::TaskId, task::TaskEvent};
use serde_json::{Value, json};

use crate::{
    MuseLink, MuseService as _,
    wire::{Body, Frame, Header, Request, Response},
};

use super::{Fake, Vm, assert_catalogue, encode_control};

pub(crate) struct Chat {
    pub(crate) stream: i64,
    pub(crate) session: String,
}

impl Chat {
    pub(crate) async fn emit(&self, vm: &mut Vm, seq: u64, name: &str, payload: Value) {
        vm.chunk(self.stream, event(&self.session, seq, name, payload), false)
            .await;
    }
}

pub(crate) async fn connect(fake: &Fake, link: &MuseLink) -> (Vm, i64) {
    fake.lookup().await;
    register(fake.vm().await, link).await
}

pub(crate) async fn register(mut vm: Vm, link: &MuseLink) -> (Vm, i64) {
    let control = vm.read_control_request().await;
    let register = vm.read_register(control).await;
    assert_catalogue(&register);
    vm.control_response(control, 200).await;
    vm.register_ack(control, &register["id"], true).await;
    assert_eq!(link.wait_registered().await, Ok(()));
    (vm, control)
}

pub(crate) async fn send_request(vm: &mut Vm) -> (i64, Value) {
    let (id, request) = request(vm, "/chat/stream").await;
    let json: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(json["device_id"], "gadget-fixture");
    assert_eq!(json["output_modality"], "text");
    (id, json)
}

pub(crate) async fn send_ack(vm: &mut Vm, id: i64, session: &str) {
    vm.response(
        id,
        200,
        json!({"result":{"session_id":session,"is_thread":true,"message_id":"user-1"}}),
        true,
    )
    .await;
}

pub(crate) async fn subscribe(vm: &mut Vm, session: &str) -> Chat {
    let (stream, request) = request(vm, "/chat/subscribe").await;
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body, json!({"session_id":session}));
    assert!(request.headers.contains(&Header {
        key: "Accept".into(),
        value: "application/x-ndjson".into(),
    }));
    vm.control_response(stream, 200).await;
    Chat {
        stream,
        session: session.into(),
    }
}

async fn request(vm: &mut Vm, route: &str) -> (i64, Request) {
    let frame = vm.next_frame().await;
    let mut request = match frame.body {
        Body::Request(request) => Some(request),
        _ => None,
    }
    .expect("expected a daemon request");
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
        let (data, end_body) = match next.body {
            Body::Chunk { data, end_body } => Some((data, end_body)),
            _ => None,
        }
        .expect("expected request body chunk");
        request.body.extend(data);
        request.end_body = end_body;
    }
    (frame.id, request)
}

pub(crate) async fn invoke(
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
    vm.chunk(control_id, encode_control(&invoke), false).await;
    vm.read_register(control_id).await
}

pub(crate) async fn expect_reset(vm: &mut Vm, id: i64) {
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

pub(crate) fn event(session: &str, seq: u64, name: &str, mut payload: Value) -> Vec<u8> {
    payload["session_id"] = json!(session);
    let mut bytes = serde_json::to_vec(&json!({"type":"event","seq":seq,"event":name,"payload":payload})).unwrap();
    bytes.push(b'\n');
    bytes
}

pub(crate) fn progress(id: &TaskId, message: &str) -> TaskEvent {
    TaskEvent::Progress {
        task_id: id.clone(),
        message: message.into(),
    }
}

impl Vm {
    pub(crate) async fn response(&mut self, id: i64, status: i32, body: Value, end_body: bool) {
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

    pub(crate) async fn chunk(&mut self, id: i64, data: Vec<u8>, end_body: bool) {
        self.send(&Frame {
            id,
            body: Body::Chunk { data, end_body },
        })
        .await;
    }
}

pub(crate) async fn start_chat(
    fake: &Fake,
    link: &MuseLink,
    vm: &mut Vm,
    task: Task,
    now: impl Fn() -> u64,
) -> (TaskId, Chat) {
    let seconds = task.deadline_seconds;
    let earliest = now() + seconds;
    let (id, chat) = tokio::join!(link.submit(task), async {
        let (send, request) = send_request(vm).await;
        let session = request["session_id"].as_str().unwrap();
        let snapshot: Value = serde_json::from_slice(&std::fs::read(fake.task_path()).unwrap()).unwrap();
        let record = snapshot["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["session_id"] == session)
            .unwrap();
        let id = record["task_id"].as_str().unwrap();
        assert_eq!(record["state"], "live");
        let deadline = record["deadline_unix_seconds"].as_u64().unwrap();
        assert!((earliest..=now() + seconds).contains(&deadline));
        assert!(
            request["message"]
                .as_str()
                .unwrap()
                .contains(&format!("Task ID: {id}\n"))
        );
        send_ack(vm, send, session).await;
        subscribe(vm, session).await
    });
    (id.unwrap(), chat)
}
