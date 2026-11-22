//! Remote admission uses the same idle device reader as local wake admission.
use super::{ServeNotice, earcon::BufferedCapture, retained_link, voice_kill::Gate};
use crate::{
    Error,
    bridge_endpoint::{
        refusal,
        remote_start::{Run, Start},
        voice::{Voice, unavailable},
    },
};
use echo_contracts::endpoint::AssistantState;
use echo_link::{EchoLink, LinkEvent, LinkEvents, MicrophoneRun};
use std::{cell::Cell, future::Future, sync::Arc, time::Duration};
use tokio::sync::watch;
use tokio::time::{Instant, sleep_until, timeout_at};

type DeviceRun<L> = <<L as EchoLink>::Events as LinkEvents>::Run;

pub(super) struct Options<R> {
    silent: fn() -> R,
    buffered: fn(R, BufferedCapture) -> R,
    remote_mic: bool,
}

impl<R> Options<retained_link::Capture<R>> {
    pub(super) fn retained(remote_mic: bool) -> Self {
        Self {
            silent: || retained_link::Capture::Silent,
            buffered: retained_link::Capture::buffered,
            remote_mic,
        }
    }
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

pub(super) async fn stop(
    cancelled: impl Future<Output = ()>,
    lease: Option<watch::Receiver<()>>,
    stopped: &Cell<bool>,
) {
    if let Some(mut lease) = lease {
        tokio::select! {
            () = cancelled => {}
            _ = lease.changed() => stopped.set(true),
        }
    } else {
        cancelled.await;
    }
}

pub(super) async fn settle(client: &impl EchoLink, endpoint: Option<&Voice>, stopped: bool) -> Result<(), Error> {
    let endpoint = endpoint.ok_or(Error::Control)?;
    if stopped {
        client.end_run().await?;
        endpoint.remote_failed();
    } else if endpoint.close_requested() {
        super::earcon::preempt();
        super::earcon::idle().await?;
        client.end_run().await?;
        endpoint.close_finished();
    } else {
        return Err(Error::Control);
    }
    Ok(())
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
        let followup = start.payload.listen && options.remote_mic;
        return Ok(Some(Admission {
            microphone: (options.silent)(),
            remote: Some(start.admit(&id, followup)),
            followup,
        }));
    }
    if !options.remote_mic {
        // The configured Wyoming peer has no correlated capture: not a transient state.
        start.refuse(refusal("NOT_SUPPORTED"));
        return Ok(None);
    }
    let id = endpoint.begin_remote(AssistantState::Listening)?;
    let capture = capture((link.0, &mut *link.1), &start, (muted, gate), observe).await;
    match capture {
        Ok(Capture::Matched((microphone, frames))) if !start.cancelled() => {
            let followup = start.payload.listen;
            endpoint.remote_admitted(&id);
            Ok(Some(Admission {
                microphone: (options.buffered)(microphone, frames),
                remote: Some(start.admit(&id, false)),
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
    Matched((R, BufferedCapture)),
    Refused(echo_contracts::endpoint::Reply),
}

async fn buffer<R: MicrophoneRun>(pending: Option<&mut (R, BufferedCapture)>) -> Result<(), Error> {
    if let Some((run, frames)) = pending.filter(|(_, frames)| !frames.ended()) {
        frames.push(run.recv().await?)
    } else {
        std::future::pending().await
    }
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
    let mut issued = false;
    let refusal = loop {
        tokio::select! {
            biased;
            _ = lease.changed() => break unavailable(),
            () = gate.wait() => {
                gate.sync(observe)?;
                break refusal("DISABLED");
            }
            () = crate::bridge_endpoint::voice::ending() => break no_capture(),
            () = sleep_until(deadline) => {
                if let Some(run) = matched.take() {
                    return Ok(Capture::Matched(run));
                }
                break no_capture();
            }
            result = &mut send, if !issued => {
                if !matches!(result, Ok(Ok(()))) {
                    break no_capture();
                }
                issued = true;
            }
            event = events.recv() => {
                if let Some(outcome) = capture_event(event?.ok_or(Error::Wyoming)?, &request, &mut matched, muted, observe)? {
                    match outcome {
                        Capture::Refused(outcome) => break outcome,
                        admitted => return Ok(admitted),
                    }
                }
            }
            result = buffer(matched.as_mut()) => result?,
        }
    };
    if matched.is_some() {
        client.end_run().await?;
    }
    Ok(Capture::Refused(refusal))
}

fn capture_event<R>(
    event: LinkEvent<R>,
    request: &str,
    matched: &mut Option<(R, BufferedCapture)>,
    muted: &mut bool,
    observe: &mut impl FnMut(ServeNotice) -> Result<(), Error>,
) -> Result<Option<Capture<R>>, Error> {
    match event {
        LinkEvent::RemoteCapture { request_id, run } if request_id == request => {
            *matched = Some((run, BufferedCapture::default()));
        }
        LinkEvent::Wake(run) if matched.is_none() => return Ok(Some(Capture::Wake(run))),
        LinkEvent::Mute(value) => {
            *muted = value;
            observe(ServeNotice::Device(LinkEvent::Mute(value)))?;
            if value {
                return Ok(Some(Capture::Refused(refusal("MUTED"))));
            }
        }
        LinkEvent::RemoteCapture { .. } | LinkEvent::StaleRemoteCapture => {
            observe(ServeNotice::Device(LinkEvent::StaleRemoteCapture))?;
        }
        LinkEvent::MicrophoneDropped(count) => observe(ServeNotice::Device(LinkEvent::MicrophoneDropped(count)))?,
        LinkEvent::Wake(_) | LinkEvent::Played | LinkEvent::Other => {}
    }
    Ok(None)
}

fn no_capture() -> echo_contracts::endpoint::Reply {
    refusal("NOT_READY")
}

// Text admission and its first reply never wait on capture. Offer one
// negotiated device run at the point where follow-up becomes eligible.
pub(super) async fn followup<L: EchoLink>(
    client: &L,
    events: &mut L::Events,
    cancelled: impl Future<Output = ()>,
    deadline: Instant,
    mut observe: impl FnMut(&LinkEvent<DeviceRun<L>>) -> Result<(), Error>,
) -> Result<Option<DeviceRun<L>>, Error> {
    let request: Arc<str> = echo_mcp_auth::new_request_id().map_err(|_| Error::Unavailable)?.into();
    let work = async {
        if client
            .request_remote_capture(Arc::downgrade(&request), deadline.into_std())
            .await
            .is_err()
        {
            return Ok(None);
        }
        loop {
            let event = events.recv().await?.ok_or(Error::Wyoming)?;
            match event {
                LinkEvent::RemoteCapture { request_id, run } if request_id == request.as_ref() => return Ok(Some(run)),
                LinkEvent::Mute(true) => {
                    observe(&event)?;
                    return Ok(None);
                }
                _ => observe(&event)?,
            }
        }
    };
    tokio::select! {
        biased;
        () = cancelled => Err(Error::Control),
        () = crate::bridge_endpoint::voice::ending() => Ok(None),
        result = timeout_at(deadline, work) => result.unwrap_or(Ok(None)),
    }
}
