mod turn;

use echo_contracts::{ids::TaskId, secret::Secret};
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::{
    ServiceError,
    tasks::{Submission, Tasks},
    wire::{Header, InvalidWire, Request},
};

pub(crate) use turn::Turn;

pub(crate) enum Command {
    Submit(Submission, oneshot::Sender<Result<(), ServiceError>>),
    Cancel { id: TaskId, session: String },
}

pub(crate) fn send(
    tasks: &Tasks,
    node: &Secret<String>,
    session: &str,
    message: &str,
) -> Result<Request, ServiceError> {
    request(
        tasks,
        "/chat/stream",
        &json!({
            "message": message,
            "device_id": node.expose(),
            "output_modality": "text",
            "session_id": session,
        }),
    )
}

pub(crate) fn subscribe(tasks: &Tasks, session: &str) -> Result<Request, ServiceError> {
    let mut request = request(tasks, "/chat/subscribe", &json!({"session_id": session}))?;
    request.headers.push(header("Accept", "application/x-ndjson".into()));
    Ok(request)
}

fn request(tasks: &Tasks, path: &str, value: &Value) -> Result<Request, ServiceError> {
    Ok(Request {
        verb: "POST".into(),
        path: path.into(),
        headers: vec![
            header("Content-Type", "application/json".into()),
            header("x-request-id", tasks.uuid()?),
            header("x-app-id", "musegadget".into()),
        ],
        body: serde_json::to_vec(value).map_err(|_| ServiceError::PeerUnavailable)?,
        end_body: true,
    })
}

fn header(key: &str, value: String) -> Header {
    Header { key: key.into(), value }
}

pub(crate) fn acknowledge(bytes: &[u8], session: String) -> Result<Turn, InvalidWire> {
    let ack: Value = serde_json::from_slice(bytes).map_err(|_| InvalidWire)?;
    let ack = ack.get("result").unwrap_or(&ack);
    if ack["session_id"] != session || ack["is_thread"] != true {
        return Err(InvalidWire);
    }
    let users = ["message_id", "reply_to_message_id"]
        .into_iter()
        .filter_map(|key| ack[key].as_str())
        .filter(|id| !id.is_empty() && id.len() <= 512)
        .map(str::to_owned)
        .collect();
    Ok(Turn::new(session, users))
}
