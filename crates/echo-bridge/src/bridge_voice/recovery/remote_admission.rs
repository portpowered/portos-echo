//! Remote admission uses the same idle device reader as local wake admission.
use super::{ServeNotice, voice_kill::Gate};
use crate::{
    Error,
    bridge_endpoint::{
        refusal,
        remote_start::{Run, Start},
        voice::{Voice, unavailable},
    },
};
use echo_contracts::endpoint::AssistantState;
use echo_link::{EchoLink, LinkEvent, LinkEvents};
use std::{sync::Arc, time::Duration};
use tokio::time::{sleep_until, timeout_at};

type DeviceRun<L> = <<L as EchoLink>::Events as LinkEvents>::Run;

pub(super) struct Options<R> {
    pub(super) silent: fn() -> R,
    pub(super) remote_mic: bool,
}

pub(super) struct Admission<R> {
    pub(super) microphone: R,
    pub(super) remote: Option<Run>,
    pub(super) followup: bool,
}

impl<R> Admission<R> {
    pub(super) fn wake(microphone: R) -> Self {
        Self {
            microphone,
            remote: None,
            followup: true,
        }
    }
}

pub(super) async fn next(endpoint: Option<&Voice>, enabled: bool) -> Start {
    if let Some(endpoint) = endpoint.filter(|_| enabled) {
        endpoint.next_start().await
    } else {
        std::future::pending().await
    }
}

pub(super) async fn prepare<L: EchoLink>(
    link: (&L, &mut L::Events),
    (endpoint, start): (&Voice, Start),
    options: &Options<DeviceRun<L>>,
    policy: (&mut bool, &mut Gate<'_>),
    observe: &mut impl FnMut(ServeNotice) -> Result<(), Error>,
) -> Result<Option<Admission<DeviceRun<L>>>, Error> {
    if start.cancelled() {
        return Ok(None);
    }
    let (muted, gate) = policy;
    gate.sync(observe)?;
    if let Some(outcome) = endpoint.start_refusal() {
        start.refuse(outcome);
        return Ok(None);
    }
    if start.payload.initial_text.is_some() {
        let id = endpoint.begin_remote(AssistantState::Thinking)?;
        return Ok(Some(Admission {
            microphone: (options.silent)(),
            remote: Some(start.admit(&id)),
            followup: false,
        }));
    }
    if !options.remote_mic {
        start.refuse(no_capture());
        return Ok(None);
    }
    let id = endpoint.begin_remote(AssistantState::Listening)?;
    let capture = capture((link.0, &mut *link.1), &start, (muted, gate), observe).await;
    match capture {
        Ok(Capture::Matched(microphone)) if !start.cancelled() => {
            let followup = start.payload.listen;
            Ok(Some(Admission {
                microphone,
                remote: Some(start.admit(&id)),
                followup,
            }))
        }
        other => {
            endpoint.remote_failed();
            match other? {
                Capture::Wake(microphone) => {
                    start.refuse(no_capture());
                    Ok(Some(Admission::wake(microphone)))
                }
                Capture::Refused(outcome) => {
                    start.refuse(outcome);
                    Ok(None)
                }
                Capture::Matched(_) => {
                    link.0.end_run().await?;
                    Ok(None)
                }
            }
        }
    }
}

enum Capture<R> {
    Wake(R),
    Matched(R),
    Refused(serde_json::Value),
}

async fn capture<L: EchoLink>(
    (client, events): (&L, &mut L::Events),
    start: &Start,
    (muted, gate): (&mut bool, &mut Gate<'_>),
    observe: &mut impl FnMut(ServeNotice) -> Result<(), Error>,
) -> Result<Capture<DeviceRun<L>>, Error> {
    let request: Arc<str> = echo_mcp_auth::new_request_id().map_err(|_| Error::Unavailable)?.into();
    let deadline = start.received + Duration::from_secs(2);
    let send = timeout_at(
        deadline,
        client.request_remote_capture(Arc::downgrade(&request), deadline.into_std()),
    );
    tokio::pin!(send);
    let mut lease = start.lease();
    let mut matched = None;
    let mut requested = false;
    let refusal = loop {
        tokio::select! {
            biased;
            _ = lease.changed() => break unavailable(),
            () = gate.wait() => {
                gate.sync(observe)?;
                break refusal("DISABLED", "The voice assistant is disabled.");
            }
            () = crate::bridge_endpoint::voice::ending() => break no_capture(),
            () = sleep_until(deadline) => {
                if let Some(run) = matched.take() {
                    return Ok(Capture::Matched(run));
                }
                break no_capture();
            }
            result = &mut send, if !requested => {
                if !matches!(result, Ok(Ok(()))) {
                    break no_capture();
                }
                requested = true;
            }
            event = events.recv() => match event?.ok_or(Error::Wyoming)? {
                LinkEvent::RemoteCapture { request_id, run } if request_id == &*request => matched = Some(run),
                LinkEvent::Wake(run) if matched.is_none() => return Ok(Capture::Wake(run)),
                LinkEvent::Mute(value) => {
                    *muted = value;
                    observe(ServeNotice::Device(LinkEvent::Mute(value)))?;
                    if value { break refusal("MUTED", "The microphone is muted."); }
                }
                LinkEvent::RemoteCapture { .. } | LinkEvent::StaleRemoteCapture => {
                    observe(ServeNotice::Device(LinkEvent::StaleRemoteCapture))?;
                }
                LinkEvent::MicrophoneDropped(count) => observe(ServeNotice::Device(LinkEvent::MicrophoneDropped(count)))?,
                LinkEvent::Wake(_) | LinkEvent::Played | LinkEvent::Other => {}
            }
        }
    };
    if matched.is_some() {
        client.end_run().await?;
    }
    Ok(Capture::Refused(refusal))
}

fn no_capture() -> serde_json::Value {
    refusal("NOT_READY", "Device microphone start is unavailable.")
}
