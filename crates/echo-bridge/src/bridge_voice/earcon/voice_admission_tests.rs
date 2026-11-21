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
    for (seconds, zone) in [
        (0, None),
        (86_400, None),
        (1_794_918_896, Some("Europe/Berlin")),
        (1_795_005_296, Some("Europe/Berlin")),
    ] {
        options.call.time_zone = zone;
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
        let started = tokio::sync::Notify::new();
        let mut completed = 0;
        let mut peer = async || Ok(MemoryPeer);
        let mut observe = super::super::voice_config::tests::retained_observer(&observer, &started, &mut completed);
        let work = admitted(
            (&client, &mut events, run),
            &mut io,
            &mut options,
            &mut peer,
            std::future::pending(),
            &mut observe,
            (
                None,
                || {
                    samples.set(samples.get() + 1);
                    now
                },
                None,
            ),
        );
        let remote = check_context(
            (server, &observer, &started),
            (
                super::super::voice_config::call_instructions("configured\n\u{8a9e} \n", now, zone).unwrap(),
                super::super::voice_config::call_instructions("live\n\u{8a9e} \n", now, zone).unwrap(),
            ),
        );
        let (result, ()) = tokio::join!(work, remote);
        drop(observe);
        assert_eq!((result, completed), (Ok(()), 2));
        assert_eq!(options.call.instructions, "configured\n\u{8a9e} \n");
        assert_eq!(options.call.prompt, "live\n\u{8a9e} \n");
    }
    assert_eq!(samples.get(), 4);
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
            (
                None,
                || {
                    samples.set(samples.get() + 1);
                    std::time::UNIX_EPOCH + Duration::from_hours(70_389_528)
                },
                None,
            ),
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
    assert_eq!(result.unwrap().unwrap().wake_phrase(), "unmuted fixture");
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
    assert_eq!(run.unwrap().wake_phrase(), "admit");
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
        assert!(matches!(result, Err(error) if error == if cancelled { Error::Control } else { Error::Unavailable }));
        assert_eq!(observer.commands(), [Command::Connect]);
    }
}

// T1: no RPC/peer/device admission when an idle settings restart wins.
#[tokio::test(start_paused = true)]
async fn settings_restart_idle_stops_before_queued_wake_and_shutdown_has_priority() {
    for cancelled in [false, true] {
        let restart = crate::bridge_settings::settings_restart::Restart::default();
        restart.request();
        let (stream, _server) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(stream);
        let mut io = RpcIo::new(read, write, echo_codex::codex_rpc::Limits::default()).unwrap();
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        observer.wake("must remain queued", vec![]);
        let result = restart
            .scope(serve(
                &mut client,
                &mut io,
                options(),
                async || -> Result<MemoryPeer, Error> { unreachable!("restart admitted a peer") },
                async {
                    if !cancelled {
                        std::future::pending::<()>().await;
                    }
                },
                |notice| {
                    assert!(matches!(notice, ServeNotice::Ready));
                    Ok(())
                },
            ))
            .await;
        assert_eq!(result, if cancelled { Err(Error::Control) } else { Ok(()) });
        assert_eq!(observer.commands(), [Command::Connect]);
        assert!(restart.pending());
    }
}

#[tokio::test(start_paused = true)]
async fn wake_earcon_enabled_service_refuses_muted_killed_and_unconfirmed_traffic() {
    for refusal in ["muted", "kill", "unconfirmed"] {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("kill");
        if refusal == "kill" {
            std::fs::write(&marker, b"invalid marker").unwrap();
        }
        let (stream, _server) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(stream);
        let mut io = RpcIo::new(read, write, echo_codex::codex_rpc::Limits::default()).unwrap();
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut options = options();
        options.session.muted = refusal == "muted";
        options.kill_path = Some(&marker);
        if refusal != "unconfirmed" {
            observer.wake("refused", vec![]);
        }
        observer.microphone_dropped(7);
        let stop = tokio::sync::Notify::new();
        let mut drops = 0;
        let result = service(
            &mut client,
            &mut io,
            options,
            async || -> Result<MemoryPeer, Error> { unreachable!("refused wake created peer") },
            stop.notified(),
            |notice| {
                match notice {
                    ServeNotice::Probe => stop.notify_one(),
                    ServeNotice::Device(LinkEvent::MicrophoneDropped(count)) => drops += count,
                    ServeNotice::Ready | ServeNotice::Kill(true) => (),
                    _ => unreachable!("refused wake admitted or played"),
                }
                Ok(())
            },
            (None, None, true, None, None),
        )
        .await;
        assert_eq!(result, Err(Error::Control));
        assert_eq!(drops, 7);
        let mut expected = vec![Command::Connect];
        if refusal == "kill" {
            expected.push(Command::FailRun(echo_link::RunFailure::KillSwitch));
        }
        expected.push(Command::Probe);
        assert_eq!(observer.commands(), expected);
    }
}

#[tokio::test(start_paused = true)]
async fn wake_earcon_admission_peer_deadline_cuts_without_success_or_connected() {
    let (stream, _server) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(stream);
    let mut io = RpcIo::new(read, write, echo_codex::codex_rpc::Limits::default()).unwrap();
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    observer.wake("deadline", vec![]);
    let LinkEvent::Wake(run) = events.recv().await.unwrap().unwrap() else {
        unreachable!()
    };
    let mut options = options();
    options.connect_timeout = Duration::from_millis(40);
    let mut listening = 0;
    let mut peer = async || std::future::pending::<Result<MemoryPeer, Error>>().await;
    let mut observe = |notice| {
        assert!(matches!(notice, ServeNotice::Listening));
        listening += 1;
        Ok(())
    };
    let work = admitted(
        (&client, &mut events, run),
        &mut io,
        &mut options,
        &mut peer,
        std::future::pending(),
        &mut observe,
        (None, || std::time::UNIX_EPOCH, None),
    );
    let remote = async {
        while !observer.commands().iter().any(|c| matches!(c, Command::Cut(_))) {
            tokio::time::advance(Duration::from_millis(20)).await;
            tokio::task::yield_now().await;
        }
        observer.finish(Ok(()));
    };
    let (result, ()) = tokio::join!(super::super::earcon::scope(&client, true, work), remote);
    assert_eq!(result, Err(Error::Deadline));
    assert_eq!(listening, 1);
    assert_eq!(
        observer
            .commands()
            .iter()
            .filter(|c| matches!(c, Command::Cut(_)))
            .count(),
        1
    );
    assert_eq!(
        observer
            .commands()
            .iter()
            .filter(|c| matches!(c, Command::Start(..)))
            .count(),
        1
    );
}

#[cfg(test)]
pub(crate) async fn endpoint_fixture(
    connector: impl echo_link::LinkConnector,
    stream: tokio::io::DuplexStream,
    cancelled: impl Future<Output = ()>,
    ready: &tokio::sync::Notify,
) -> Result<(), Error> {
    use echo_link::LinkConnector as _;
    let (read, write) = tokio::io::split(stream);
    let mut io = RpcIo::new(read, write, echo_codex::codex_rpc::Limits::default()).unwrap();
    let retained = super::super::retained_link::Retained::new(connector);
    let mut link = retained.connect().await.unwrap();
    link.enable_capture(30).await.unwrap();
    let remote = remote_admission::Options::retained(true);
    let result = service(
        &mut link,
        &mut io,
        options(),
        async || Ok(MemoryPeer),
        cancelled,
        |notice| {
            if matches!(notice, ServeNotice::Ready) {
                ready.notify_one();
            }
            Ok(())
        },
        (None, None, false, None, Some(remote)),
    )
    .await;
    retained.close().await;
    result
}
