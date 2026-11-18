//! Idle supervision and first-wake admission on the authenticated IO borrower.
use super::{ConversationOptions, Notice, SessionNotice, SessionOptions, open_conversation, voice_kill::Gate};
use crate::{Error, bridge_codex};
use echo_codex::{DynamicTool, codex_io::RpcIo, codex_protocol::Message};
use echo_link::{EchoLink, LinkEvent, LinkEvents};
use echo_media::MediaPeer;
use std::{future::Future, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::{Instant, timeout, timeout_at},
};

pub struct ServeOptions<'a> {
    pub kill_path: Option<&'a std::path::Path>,
    pub call: bridge_codex::Options<'a>,
    pub tools: Vec<DynamicTool>,
    pub session: SessionOptions,
    pub connect_timeout: Duration,
}

impl<'o> ServeOptions<'o> {
    pub(super) fn conversation<'a>(&self, instructions: &'a str, deadline: Instant) -> ConversationOptions<'a>
    where
        'o: 'a,
    {
        ConversationOptions {
            call: bridge_codex::Options {
                voice: self.call.voice,
                instructions,
                model: self.call.model,
            },
            tools: self.tools.clone(),
            deadline,
            session: self.session,
        }
    }
}

/// Ready means authenticated device readiness, never media connectivity.
pub enum ServeNotice {
    Ready,
    Kill(bool),
    Listening,
    Probe,
    Device(LinkEvent<()>),
    Session(SessionNotice),
}

/// Serve successive conversations inside `with_device`'s authenticated callback.
/// Take device events once and keep driving process IO before the first wake
/// and between calls. A new peer is created only for an unmuted admitted wake;
/// the conversation retains it across replies until its idle timeout.
///
/// Cancellation must be forwarded here from the host's watch receiver. Active
/// call/media cleanup is awaited before returning to device close/child reaping.
/// A failed conversation terminates this borrower; no replay or restart occurs.
///
/// # Errors
/// Fixed device/control/media/observer failures and invalid budgets. The caller
/// must await the outer authenticated host even on failure or cancellation.
pub async fn serve<L, R, W, P>(
    client: &mut L,
    io: &mut RpcIo<R, W>,
    options: ServeOptions<'_>,
    peer: impl AsyncFnMut() -> Result<P, Error>,
    cancelled: impl Future<Output = ()>,
    observe: impl FnMut(ServeNotice) -> Result<(), Error>,
) -> Result<(), Error>
where
    L: EchoLink,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    P: MediaPeer,
{
    service(client, io, options, peer, cancelled, observe, None).await
}

pub(super) async fn service<L, R, W, P>(
    client: &mut L,
    io: &mut RpcIo<R, W>,
    mut options: ServeOptions<'_>,
    mut peer: impl AsyncFnMut() -> Result<P, Error>,
    cancelled: impl Future<Output = ()>,
    mut observe: impl FnMut(ServeNotice) -> Result<(), Error>,
    mut tools: Option<&mut super::voice_tools::Tools>,
) -> Result<(), Error>
where
    L: EchoLink,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    P: MediaPeer,
{
    super::voice_session::validate(&options.session)?;
    if options.connect_timeout.is_zero() || Instant::now().checked_add(options.connect_timeout).is_none() {
        return Err(Error::Input);
    }
    let mut events = client.events()?;
    tokio::pin!(cancelled);
    let mut gate = Gate::new(options.kill_path);
    loop {
        gate.sync(&mut observe)?;
        observe(ServeNotice::Ready)?;
        let microphone = wait(
            client,
            &mut events,
            io,
            (&mut options.session.muted, &mut gate),
            cancelled.as_mut(),
            &mut observe,
        )
        .await?;
        let killed = std::cell::Cell::new(false);
        let result = Box::pin(admitted(
            (&*client, &mut events, microphone),
            io,
            &mut options,
            &mut peer,
            async {
                tokio::select! {
                    biased;
                    () = cancelled.as_mut() => (),
                    () = gate.wait() => killed.set(true),
                }
            },
            &mut observe,
            (tools.as_deref_mut(), std::time::SystemTime::now),
        ))
        .await;
        if killed.get() && result == Err(Error::Control) {
            // Cancelled negotiation can invalidate IO. Never resume that
            // generation or present readiness after an unsuccessful stop.
            timeout(Duration::from_secs(2), io.flush())
                .await
                .map_err(|_| Error::Deadline)?
                .map_err(bridge_codex::rpc_error)?;
            client.fail_run(echo_link::RunFailure::KillSwitch).await?;
            gate.tripped(&mut observe)?;
        } else {
            result?;
        }
    }
}

async fn admitted<L, R, W, P>(
    device: (&L, &mut L::Events, <L::Events as LinkEvents>::Run),
    io: &mut RpcIo<R, W>,
    options: &mut ServeOptions<'_>,
    peer: &mut impl AsyncFnMut() -> Result<P, Error>,
    cancelled: impl Future<Output = ()>,
    observe: &mut impl FnMut(ServeNotice) -> Result<(), Error>,
    context: (
        Option<&mut super::voice_tools::Tools>,
        impl FnOnce() -> std::time::SystemTime,
    ),
) -> Result<(), Error>
where
    L: EchoLink,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    P: MediaPeer,
{
    let (tools, clock) = context;
    tokio::pin!(cancelled);
    if let Some(tools) = &tools {
        tools.budget.check_call()?;
    }
    observe(ServeNotice::Listening)?;
    let deadline = Instant::now()
        .checked_add(options.connect_timeout)
        .ok_or(Error::Input)?;
    let (media, instructions) = tokio::select! {
        biased;
        () = cancelled.as_mut() => return Err(Error::Control),
        result = async {
            let instructions = super::voice_config::call_instructions(options.call.instructions, clock())?;
            let media = timeout_at(deadline, peer()).await.map_err(|_| Error::Deadline)??;
            Ok::<_, Error>((media, instructions))
        } => result?,
    };
    let options_call = options.conversation(&instructions, deadline);
    let mut observer = |notice| {
        if let SessionNotice::Run(Notice::Device(LinkEvent::Mute(value))) = &notice {
            options.session.muted = *value;
        }
        observe(ServeNotice::Session(notice))
    };
    if let Some(tools) = tools {
        return Box::pin(super::voice_tools::conversation(
            device,
            io,
            media,
            (options_call, tools),
            cancelled.as_mut(),
            &mut observer,
        ))
        .await
        .map(|_| ());
    }
    open_conversation(device, io, media, options_call, cancelled.as_mut(), &mut observer)
        .await
        .map(|_| ())
}

async fn wait<L: EchoLink, R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    client: &L,
    events: &mut L::Events,
    io: &mut RpcIo<R, W>,
    policy: (&mut bool, &mut Gate<'_>),
    cancelled: impl Future<Output = ()>,
    observe: &mut impl FnMut(ServeNotice) -> Result<(), Error>,
) -> Result<<L::Events as LinkEvents>::Run, Error> {
    let (muted, gate) = policy;
    tokio::pin!(cancelled);
    let mut poll = tokio::time::interval(Duration::from_millis(20));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Preserve one in-flight probe across device/control activity. Device
    // recovery belongs to the adapter; a failed probe ends this borrower.
    let probe = probe(client);
    tokio::pin!(probe);
    loop {
        tokio::select! {
            biased;
            () = cancelled.as_mut() => return Err(Error::Control),
            result = async {
                tokio::select! {
                    _ = poll.tick() => { gate.sync(observe)?; },
                    result = &mut probe => {
                        result?;
                        observe(ServeNotice::Probe)?;
                        probe.set(self::probe(client));
                    },
                    event = io.next_event() => idle_message(io, event.map_err(bridge_codex::rpc_error)?)?,
                    event = events.recv() => match event?.ok_or(Error::Wyoming)? {
                        LinkEvent::Wake(run) => {
                            if gate.sync(observe)? {
                                client.fail_run(echo_link::RunFailure::KillSwitch).await?;
                            } else if !*muted { return Ok(Some(run)); }
                        },
                        LinkEvent::Played | LinkEvent::Other => (),
                        LinkEvent::Mute(value) => {
                            *muted = value;
                            observe(ServeNotice::Device(LinkEvent::Mute(value)))?;
                        }
                        LinkEvent::MicrophoneDropped(count) => observe(ServeNotice::Device(LinkEvent::MicrophoneDropped(count)))?,
                    },
                }
                Ok::<_, Error>(None)
            } => if let Some(run) = result? { return Ok(run); },
        }
        tokio::task::yield_now().await;
    }
}

fn idle_message<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    io: &mut RpcIo<R, W>,
    message: Message,
) -> Result<(), Error> {
    if let Message::Request { id, .. } = message {
        // No thread/turn has been admitted: never execute an unsolicited tool.
        io.respond(&id, &serde_json::json!({"contentItems":[],"success":false}))
            .map_err(bridge_codex::rpc_error)?;
    }
    Ok(())
}

async fn probe(client: &impl EchoLink) -> Result<(), Error> {
    tokio::time::sleep(Duration::from_secs(1)).await;
    timeout(Duration::from_secs(5), client.probe())
        .await
        .map_err(|_| Error::Deadline)?
        .map_err(Error::from)
}

#[cfg(test)]
mod tests {
    use super::super::voice_config::tests::{MemoryPeer, admission_options as options, check_context};
    use super::*;
    use echo_link::{
        LinkConnector, MicrophoneRun,
        fake::{Command, FakeConnector, Observer},
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[tokio::test(start_paused = true)]
    async fn admission_samples_fresh_context_in_both_fields_without_accumulation() {
        let samples = std::cell::Cell::new(0);
        let mut options = options();
        for seconds in [0, 86_400] {
            let (stream, server) = tokio::io::duplex(16_384);
            let (read, write) = tokio::io::split(stream);
            let mut io = RpcIo::new(read, write, echo_codex::codex_rpc::Limits::default()).unwrap();
            let observer = Observer::default();
            let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
            let mut events = client.events().unwrap();
            observer.wake("admit", vec![]);
            let LinkEvent::Wake(run) = events.recv().await.unwrap().unwrap() else {
                unreachable!()
            };
            let now = std::time::UNIX_EPOCH + Duration::from_secs(seconds);
            let clock = || {
                samples.set(samples.get() + 1);
                now
            };
            let mut peer = async || Ok(MemoryPeer);
            let mut observe = |notice| {
                if matches!(notice, ServeNotice::Session(SessionNotice::Connected)) {
                    return Err(Error::Control);
                }
                Ok(())
            };
            let work = admitted(
                (&client, &mut events, run),
                &mut io,
                &mut options,
                &mut peer,
                std::future::pending(),
                &mut observe,
                (None, clock),
            );
            let remote = check_context(
                server,
                super::super::voice_config::call_instructions("configured\n\u{8a9e} \n", now).unwrap(),
            );
            let (result, ()) = tokio::join!(work, remote);
            assert_eq!(result, Err(Error::Control));
            assert_eq!(options.call.instructions, "configured\n\u{8a9e} \n");
        }
        assert_eq!(samples.get(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn admission_invalid_clock_and_ready_cancellation_emit_no_rpc_or_peer() {
        for cancelled in [false, true] {
            let (stream, mut server) = tokio::io::duplex(4096);
            let (read, write) = tokio::io::split(stream);
            let mut io = RpcIo::new(read, write, echo_codex::codex_rpc::Limits::default()).unwrap();
            let observer = Observer::default();
            let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
            let mut events = client.events().unwrap();
            observer.wake("admit", vec![]);
            let LinkEvent::Wake(run) = events.recv().await.unwrap().unwrap() else {
                unreachable!()
            };
            let samples = std::cell::Cell::new(0);
            let result = admitted(
                (&client, &mut events, run),
                &mut io,
                &mut options(),
                &mut async || -> Result<MemoryPeer, Error> { unreachable!() },
                async {
                    if !cancelled {
                        std::future::pending::<()>().await;
                    }
                },
                &mut |_| Ok(()),
                (None, || {
                    samples.set(samples.get() + 1);
                    std::time::UNIX_EPOCH + Duration::from_hours(70_389_528)
                }),
            )
            .await;
            assert_eq!(result, Err(if cancelled { Error::Control } else { Error::Input }));
            assert_eq!(samples.get(), usize::from(!cancelled));
            drop(io);
            let mut bytes = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut server, &mut bytes)
                .await
                .unwrap();
            assert_eq!(bytes, Vec::<u8>::new());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn idle_polls_control_refuses_unsolicited_tools_and_probes_before_wake() {
        let (stream, server) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(stream);
        let mut io = RpcIo::new(read, write, echo_codex::codex_rpc::Limits::default()).unwrap();
        let mut server = BufReader::new(server);
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = client.events().unwrap();
        let mut muted = false;
        let mut probes = 0;
        let readiness = tokio::sync::Notify::new();
        let mut observe = |notice| {
            assert!(matches!(notice, ServeNotice::Probe));
            probes += 1;
            readiness.notify_one();
            Ok(())
        };
        let mut gate = Gate::new(None);
        let work = wait(
            &client,
            &mut events,
            &mut io,
            (&mut muted, &mut gate),
            std::future::pending(),
            &mut observe,
        );
        let remote = async {
            server
                .get_mut()
                .write_all(b"{\"id\":\"unadmitted\",\"method\":\"item/tool/call\",\"params\":{}}\n")
                .await
                .unwrap();
            let mut line = String::new();
            server.read_line(&mut line).await.unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&line).unwrap(),
                serde_json::json!({"id":"unadmitted","result":{"contentItems":[],"success":false}})
            );
            readiness.notified().await;
            assert!(observer.commands().contains(&Command::Probe));
            observer.wake("unmuted fixture", vec![]);
        };
        let (result, ()) = tokio::join!(work, remote);
        assert_eq!(result.unwrap().wake_phrase(), "unmuted fixture");
        assert!(!muted);
        assert_eq!(probes, 1);
        assert_eq!(
            io.begin_request("account/read", &serde_json::json!({})).unwrap(),
            echo_codex::codex_protocol::RequestId::Number(1)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn idle_mute_blocks_wake_and_observes_drops_until_unmuted() {
        let (stream, _server) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(stream);
        let mut io = RpcIo::new(read, write, echo_codex::codex_rpc::Limits::default()).unwrap();
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = client.events().unwrap();
        observer.mute(true);
        observer.wake("discard", vec![]);
        observer.microphone_dropped(7);
        observer.mute(false);
        observer.wake("admit", vec![]);
        let mut notices = Vec::new();
        let mut muted = false;
        let run = wait(
            &client,
            &mut events,
            &mut io,
            (&mut muted, &mut Gate::new(None)),
            std::future::pending(),
            &mut |notice| {
                notices.push(match notice {
                    ServeNotice::Device(LinkEvent::Mute(true)) => "muted",
                    ServeNotice::Device(LinkEvent::Mute(false)) => "unmuted",
                    ServeNotice::Device(LinkEvent::MicrophoneDropped(7)) => "dropped",
                    _ => unreachable!(),
                });
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(run.wake_phrase(), "admit");
        assert!(!muted);
        assert_eq!(notices, ["muted", "dropped", "unmuted"]);
        assert_eq!(observer.commands(), [Command::Connect]);
    }

    #[tokio::test(start_paused = true)]
    async fn idle_eof_and_ready_cancellation_fail_without_admitting_queued_wake() {
        for cancelled in [false, true] {
            let (stream, server) = tokio::io::duplex(4096);
            let (read, write) = tokio::io::split(stream);
            let mut io = RpcIo::new(read, write, echo_codex::codex_rpc::Limits::default()).unwrap();
            let observer = Observer::default();
            let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
            let mut events = client.events().unwrap();
            if cancelled {
                observer.wake("discard", vec![]);
            }
            drop(server);
            let result = wait(
                &client,
                &mut events,
                &mut io,
                (&mut false, &mut Gate::new(None)),
                async {
                    if !cancelled {
                        std::future::pending::<()>().await;
                    }
                },
                &mut |_| unreachable!(),
            )
            .await;
            assert!(
                matches!(result, Err(error) if error == if cancelled { Error::Control } else { Error::Unavailable })
            );
            assert_eq!(observer.commands(), [Command::Connect]);
        }
    }
}
