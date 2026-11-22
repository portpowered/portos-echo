use std::{collections::HashMap, future::pending};

use echo_contracts::{ids::TaskId, secret::Secret};
use tokio::sync::oneshot;

use crate::{
    ServiceError,
    chat::{self, Command, Turn},
    tasks::{Submission, Tasks},
    wire::{Body, InvalidWire},
};

use super::{ControlLink, Socket};

const MAX_STREAMS: usize = 64;
const ACK_SECONDS: u64 = 15;
const MAX_ACK: usize = 256 * 1024;
type Receipt = oneshot::Sender<Result<(), ServiceError>>;

#[derive(Default)]
pub(super) struct Chats(HashMap<i64, Chat>);

enum Chat {
    Ack(Ack),
    Follow { id: TaskId, deadline: u64, turn: Turn },
}

impl Chat {
    fn id(&self) -> &TaskId {
        match self {
            Self::Ack(ack) => &ack.id,
            Self::Follow { id, .. } => id,
        }
    }

    fn wake_at(&self) -> u64 {
        match self {
            Self::Ack(ack) => ack.expires,
            Self::Follow { deadline, .. } => *deadline,
        }
    }
}

impl Chats {
    pub(super) async fn command<S: Socket>(
        &mut self,
        link: &mut ControlLink<S>,
        tasks: &Tasks,
        node: &Secret<String>,
        command: Command,
    ) -> Result<(), ServiceError> {
        match command {
            Command::Submit(submission, receipt) => self.submit(link, tasks, node, submission, receipt).await,
            Command::Cancel { id, session } => {
                self.end_task(link, &id).await?;
                let message = format!("Cancel task {id}. Do not call echo.report.result for it.");
                let request = chat::send(tasks, node, &session, &message)?;
                link.send_advisory(request).await
            }
        }
    }

    async fn submit<S: Socket>(
        &mut self,
        link: &mut ControlLink<S>,
        tasks: &Tasks,
        node: &Secret<String>,
        submission: Submission,
        receipt: Receipt,
    ) -> Result<(), ServiceError> {
        let Submission {
            id,
            session,
            deadline,
            message,
        } = submission;
        let ready = match tasks.live(&id) {
            Ok(live) => live && self.0.len() < MAX_STREAMS,
            Err(error) => {
                let _ = receipt.send(Err(error));
                return Ok(());
            }
        };
        let request = ready.then(|| chat::send(tasks, node, &session, &message)).transpose();
        let Ok(Some(request)) = request else {
            refuse(tasks, &id, receipt);
            return Ok(());
        };
        let stream = link.open_request(request).await?;
        let expires = deadline.min(tasks.clock.now().saturating_add(ACK_SECONDS));
        self.0.insert(
            stream,
            Chat::Ack(Ack {
                id,
                session,
                deadline,
                expires,
                receipt,
                bytes: Vec::new(),
            }),
        );
        Ok(())
    }

    pub(super) fn deadline(&self) -> Option<u64> {
        self.0.values().map(Chat::wake_at).min()
    }

    pub(super) async fn wait(tasks: &Tasks, deadline: Option<u64>) {
        match deadline {
            Some(deadline) => tasks.clock.wait_until(deadline).await,
            None => pending().await,
        }
    }

    pub(super) async fn expire<S: Socket>(
        &mut self,
        link: &mut ControlLink<S>,
        tasks: &Tasks,
    ) -> Result<(), ServiceError> {
        let now = tasks.clock.now();
        let expired: Vec<_> = self
            .0
            .iter()
            .filter(|(_, chat)| now >= chat.wake_at())
            .map(|(&id, _)| id)
            .collect();
        for stream in expired {
            if let Some(chat) = self.0.remove(&stream) {
                // A send can time out before the task deadline. That leaves the task
                // live; only its absolute deadline is allowed to produce a failure.
                let _ = tasks.live(chat.id());
            }
            link.reset(stream).await?;
        }
        Ok(())
    }

    pub(super) async fn end_task<S: Socket>(
        &mut self,
        link: &mut ControlLink<S>,
        id: &TaskId,
    ) -> Result<(), ServiceError> {
        if let Some((&stream, _)) = self.0.iter().find(|(_, chat)| chat.id() == id) {
            self.0.remove(&stream);
            link.reset(stream).await?;
        }
        Ok(())
    }

    pub(super) async fn receive<S: Socket>(
        &mut self,
        link: &mut ControlLink<S>,
        tasks: &Tasks,
        stream: i64,
        incoming: Result<Body, InvalidWire>,
    ) -> Result<(), ServiceError> {
        let Some(chat) = self.0.remove(&stream) else {
            return Ok(());
        };
        let Some(body) = incoming.ok().and_then(Data::from_body) else {
            // Reset or protocol interruption leaves an already-admitted task pending.
            return Ok(());
        };
        match chat {
            Chat::Ack(ack) => self.ack(link, tasks, stream, ack, body).await,
            Chat::Follow { id, deadline, mut turn } => {
                if follow(tasks, &id, &mut turn, &body) {
                    self.0.insert(stream, Chat::Follow { id, deadline, turn });
                    Ok(())
                } else {
                    link.reset(stream).await
                }
            }
        }
    }

    async fn ack<S: Socket>(
        &mut self,
        link: &mut ControlLink<S>,
        tasks: &Tasks,
        stream: i64,
        mut ack: Ack,
        body: Data,
    ) -> Result<(), ServiceError> {
        if !body.ok || ack.bytes.len() + body.bytes.len() > MAX_ACK {
            refuse(tasks, &ack.id, ack.receipt);
            return link.reset(stream).await;
        }
        ack.bytes.extend(body.bytes);
        if !body.end {
            self.0.insert(stream, Chat::Ack(ack));
            return Ok(());
        }
        let Ok(turn) = chat::acknowledge(&ack.bytes, ack.session) else {
            refuse(tasks, &ack.id, ack.receipt);
            return Ok(());
        };
        let _ = ack.receipt.send(Ok(()));
        if tasks.live(&ack.id) == Ok(true) {
            let subscribe = chat::subscribe(tasks, turn.session())?;
            let stream = link.open_request(subscribe).await?;
            self.0.insert(
                stream,
                Chat::Follow {
                    id: ack.id,
                    deadline: ack.deadline,
                    turn,
                },
            );
        }
        Ok(())
    }
}

struct Ack {
    id: TaskId,
    session: String,
    deadline: u64,
    expires: u64,
    receipt: Receipt,
    bytes: Vec<u8>,
}

struct Data {
    ok: bool,
    bytes: Vec<u8>,
    end: bool,
}

impl Data {
    fn from_body(body: Body) -> Option<Self> {
        match body {
            Body::Response(r) => Some(Self {
                ok: (200..300).contains(&r.status),
                bytes: r.body,
                end: r.end_body,
            }),
            Body::Chunk { data, end_body } => Some(Self {
                ok: true,
                bytes: data,
                end: end_body,
            }),
            _ => None,
        }
    }
}

fn refuse(tasks: &Tasks, id: &TaskId, receipt: Receipt) {
    let result = tasks
        .refuse(id)
        .and_then(|terminal| terminal.then_some(()).ok_or(ServiceError::PeerUnavailable));
    let _ = receipt.send(result);
}

fn follow(tasks: &Tasks, id: &TaskId, turn: &mut Turn, body: &Data) -> bool {
    if !body.ok {
        return false;
    }
    let mut bytes = body.bytes.as_slice();
    loop {
        match turn.next(&mut bytes, body.end) {
            Ok(Some(event)) => {
                let Ok(progress) = turn.progress(&event) else {
                    return false;
                };
                if progress.is_some_and(|text| tasks.progress(id, text) != Ok(true)) {
                    return false;
                }
            }
            Ok(None) => return !body.end && !turn.finished(),
            Err(_) => return false,
        }
    }
}
