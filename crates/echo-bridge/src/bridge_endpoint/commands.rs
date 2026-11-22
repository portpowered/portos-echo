//! Correlate gateway command frames before they reach a local owner.
//!
//! A frame's `header.id` is the command id; a synchronous command is answered by
//! one report whose `requestId` is that id. Speaker commands are fire-and-forget.
use super::snapshot::Camera;
use crate::bridge_trace::Trace;
use echo_contracts::endpoint::{EndSession, Frame, FrameHeader, Reply, Report, StartSession, VERSION};
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::time::Instant;

const RECENT_COMMANDS: usize = 256;

#[derive(Default)]
pub(super) struct Receipts(HashMap<String, Instant>);

pub(super) struct Command {
    header: FrameHeader,
    body: Value,
    received: Instant,
}

pub(super) enum Prepared<T> {
    Reply(T),
    Snapshot { body: T, timeout: T, received: Instant },
}

impl Receipts {
    /// Accept one frame addressed to this device, once per command id.
    pub(super) fn accept(
        &mut self,
        device_id: &str,
        text: &str,
        received: Instant,
        deadline: Instant,
    ) -> Option<Command> {
        let Frame { header, body, context } = serde_json::from_str(text).ok()?;
        if context.device_id != device_id
            || header.version != VERSION
            || header.id.trim().is_empty()
            || header.name.trim().is_empty()
            || Instant::now() >= deadline
        {
            return None;
        }
        self.0.retain(|_, expires| *expires > received);
        if self.0.len() >= RECENT_COMMANDS || self.0.contains_key(&header.id) {
            return None;
        }
        self.0.insert(header.id.clone(), deadline);
        Some(Command { header, body, received })
    }
}

impl Command {
    /// Prepare the command's report, or return `None` for a fire-and-forget command.
    pub(super) async fn reply<T>(
        self,
        lease: tokio::sync::watch::Receiver<()>,
        camera: &Camera,
        trace: Option<&Trace>,
        prepare: impl Fn(Report) -> T,
    ) -> Option<Prepared<T>> {
        let Self { header, body, received } = self;
        let name = audit_name(&header);
        match (header.interface(), header.name.as_str()) {
            (Some("speaker"), "set-volume" | "adjust-volume" | "set-mute") => {
                let code = match super::speaker::command(&header.name, body, received).await {
                    super::speaker::Outcome::Sent => "OK",
                    super::speaker::Outcome::Invalid => "INVALID_REQUEST",
                    super::speaker::Outcome::Dropped(reason) => {
                        super::note(trace, "endpoint.speaker_dropped", json!({ "reason": reason }));
                        "DROPPED"
                    }
                };
                audit(trace, name, code, received);
                None
            }
            (Some("snapshot"), "get-snapshot") => {
                let prepared = camera
                    .reply(body, received, trace, |snapshot, sampled_at_ms| {
                        let mut report = Report::reply(&header.id, Reply::Snapshot(snapshot));
                        report.sampled_at_ms = Some(sampled_at_ms);
                        prepare(report)
                    })
                    .await;
                Some(match prepared {
                    Ok(body) => Prepared::Snapshot {
                        body,
                        timeout: prepare(Report::reply(&header.id, refusal("TIMEOUT"))),
                        received,
                    },
                    Err(reply) => {
                        audit(trace, name, reply.code(), received);
                        Prepared::Reply(prepare(Report::reply(&header.id, reply)))
                    }
                })
            }
            _ => {
                let reply = correlate(&header, body, received, lease).await;
                audit(trace, name, reply.code(), received);
                Some(Prepared::Reply(prepare(Report::reply(&header.id, reply))))
            }
        }
    }
}

fn audit_name(header: &FrameHeader) -> &str {
    match (header.interface(), header.name.as_str()) {
        (Some("assistant-session"), "start-session" | "end-session")
        | (Some("speaker"), "set-volume" | "adjust-volume" | "set-mute")
        | (Some("snapshot"), "get-snapshot") => &header.name,
        _ => "unsupported",
    }
}

pub(super) fn audit(trace: Option<&Trace>, name: &str, code: &str, received: Instant) {
    if let Some(trace) = trace {
        let duration_ms = u64::try_from(received.elapsed().as_millis()).unwrap_or(u64::MAX);
        let _ = trace.clone().record(
            "endpoint.command",
            None,
            json!({ "name": name, "code": code, "duration_ms": duration_ms }),
        );
    }
}

async fn correlate(
    header: &FrameHeader,
    body: Value,
    received: Instant,
    lease: tokio::sync::watch::Receiver<()>,
) -> Reply {
    match (header.interface(), header.name.as_str()) {
        (Some("assistant-session"), "start-session") if body.is_object() => {
            let Ok(start) = serde_json::from_value::<StartSession>(body) else {
                return invalid();
            };
            if let Some(voice) = super::voice::Voice::current() {
                voice.start(start, received, lease).await
            } else {
                super::voice::unavailable()
            }
        }
        (Some("assistant-session"), "end-session") if body.is_object() => {
            let Ok(end) = serde_json::from_value::<EndSession>(body) else {
                return invalid();
            };
            if !end.valid() {
                return invalid();
            }
            if let Some(voice) = super::voice::Voice::current() {
                voice.end(&end.session_id, received).await
            } else {
                super::voice::not_found()
            }
        }
        (Some("assistant-session"), "start-session" | "end-session") => invalid(),
        _ => refusal("UNSUPPORTED_COMMAND"),
    }
}

/// A reason-code refusal; Port OS writes the message.
pub(crate) fn refusal(code: &'static str) -> Reply {
    Reply::refused(code)
}

pub(super) fn invalid() -> Reply {
    refusal("INVALID_REQUEST")
}
