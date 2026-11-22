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

struct Chat {
    id: Option<TaskId>,
    deadline: u64,
    phase: Phase,
}

enum Phase {
    Ack(Ack),
    Follow(Turn),
    Advisory,
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
                if self.0.len() < MAX_STREAMS {
                    let stream = link.open_request(request).await?;
                    let deadline = tasks.clock.now().saturating_add(ACK_SECONDS);
                    self.0.insert(
                        stream,
                        Chat {
                            id: None,
                            deadline,
                            phase: Phase::Advisory,
                        },
                    );
                }
                Ok(())
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
        let ready = self.0.len() < MAX_STREAMS && tasks.live(&id)?;
        let request = ready.then(|| chat::send(tasks, node, &session, &message)).transpose();
        let Ok(Some(request)) = request else {
            refuse(tasks, &id, receipt);
            return Ok(());
        };
        let stream = link.open_request(request).await?;
        let expires = deadline.min(tasks.clock.now().saturating_add(ACK_SECONDS));
        let phase = Phase::Ack(Ack {
            session,
            deadline,
            expires,
            receipt,
            bytes: Vec::new(),
        });
        self.0.insert(
            stream,
            Chat {
                id: Some(id),
                deadline: expires,
                phase,
            },
        );
        Ok(())
    }

    pub(super) fn deadline(&self) -> Option<u64> {
        self.0.values().map(|chat| chat.deadline).min()
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
            .filter(|(_, chat)| now >= chat.deadline)
            .map(|(&id, _)| id)
            .collect();
        for stream in expired {
            if let Some(id) = self.0.remove(&stream).and_then(|chat| chat.id) {
                // A send can time out before the task deadline. That leaves the task
                // live; only its absolute deadline is allowed to produce a failure.
                let _ = tasks.live(&id);
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
        if let Some((&stream, _)) = self.0.iter().find(|(_, chat)| chat.id.as_ref() == Some(id)) {
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
        match chat.phase {
            Phase::Ack(ack) => {
                let id = chat.id.ok_or(ServiceError::PeerUnavailable)?;
                self.ack(link, tasks, stream, id, ack, body).await
            }
            Phase::Follow(mut turn) => {
                let id = chat.id.ok_or(ServiceError::PeerUnavailable)?;
                if follow(tasks, &id, &mut turn, &body) {
                    self.0.insert(
                        stream,
                        Chat {
                            phase: Phase::Follow(turn),
                            id: Some(id),
                            ..chat
                        },
                    );
                    Ok(())
                } else {
                    link.reset(stream).await
                }
            }
            Phase::Advisory => link.reset(stream).await,
        }
    }

    async fn ack<S: Socket>(
        &mut self,
        link: &mut ControlLink<S>,
        tasks: &Tasks,
        stream: i64,
        id: TaskId,
        mut ack: Ack,
        body: Data,
    ) -> Result<(), ServiceError> {
        if !body.ok || ack.bytes.len() + body.bytes.len() > MAX_ACK {
            refuse(tasks, &id, ack.receipt);
            return link.reset(stream).await;
        }
        ack.bytes.extend(body.bytes);
        if !body.end {
            self.0.insert(
                stream,
                Chat {
                    id: Some(id),
                    deadline: ack.expires,
                    phase: Phase::Ack(ack),
                },
            );
            return Ok(());
        }
        let Ok(turn) = chat::acknowledge(&ack.bytes, ack.session) else {
            refuse(tasks, &id, ack.receipt);
            return Ok(());
        };
        let _ = ack.receipt.send(Ok(()));
        if tasks.live(&id) == Ok(true) {
            let subscribe = chat::subscribe(tasks, turn.session())?;
            let stream = link.open_request(subscribe).await?;
            self.0.insert(
                stream,
                Chat {
                    id: Some(id),
                    deadline: ack.deadline,
                    phase: Phase::Follow(turn),
                },
            );
        }
        Ok(())
    }
}

struct Ack {
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
