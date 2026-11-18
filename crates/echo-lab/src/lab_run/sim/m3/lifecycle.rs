//! T3: real task joins and child collection behind the CLI's `world_closed` check.
use super::*;
use tokio::io::AsyncWriteExt;

async fn first_screen() -> (Screen, tokio::net::TcpStream) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let connect = Screen::connect(listener.local_addr().unwrap(), true);
    let serve = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.write_all(b"HTTP/1.1 200 OK\r\n\r\ndata: {\"seq\":1,\"state\":{\"state\":\"ready\",\"message\":\"Echo Live ready\"}}\n").await.unwrap();
        socket
    };
    let (screen, socket) = tokio::join!(connect, serve);
    (screen.unwrap(), socket)
}

#[tokio::test]
async fn t3_m3_first_pcm_waits_for_admission_trace_before_next_step() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("bridge.private.jsonl");
    std::fs::write(&path, "").unwrap();
    let (screen, mut socket) = first_screen().await;
    let mut world = World {
        root: root.path().into(),
        screen: Some(screen),
        first_pcm: Some(960),
        ..World::default()
    };
    let mut ready = Box::pin(world.first_started());
    std::future::poll_fn(|cx| {
        assert!(ready.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    // Model the independent send-receipt observer being scheduled after PCM.
    let record = json!({"event":"reply.started","run_id":"run-1","elapsed_ms":1});
    std::fs::write(path, format!("{record}\n")).unwrap();
    socket
        .write_all(b"data: {\"seq\":2,\"state\":{\"state\":\"speaking\"}}\n")
        .await
        .unwrap();
    assert_eq!(ready.await, Ok(()));
    assert_eq!(world.first_pcm, Some(960));
    assert_eq!(world.trace.records, [record]);
    let expected = super::super::super::selectors::parse(&[json!("reply.started")]).unwrap();
    assert_eq!(
        super::super::super::selectors::check(&world.trace.records, &expected, Some("run-1")),
        Ok(())
    );
}

#[tokio::test(start_paused = true)]
async fn t3_m3_missing_admission_presentation_keeps_original_deadline() {
    let (screen, _socket) = first_screen().await;
    let mut world = World {
        screen: Some(screen),
        ..World::default()
    };
    let mut ready = Box::pin(world.first_started());
    std::future::poll_fn(|cx| {
        assert!(ready.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    assert_eq!(
        ready.await,
        Err("M3 first reply: missing Speaking before 5000 ms deadline".into())
    );
    assert_eq!(world.trace.records, [] as [Value; 0]);
}

impl World {
    pub(super) fn record_release<T, E>(&mut self, owner: &'static str, result: &Result<T, E>) {
        if result.is_ok() {
            self.released.push(owner);
        }
    }
}

async fn listening() -> World {
    let relay = Relay::bind("127.0.0.1:1".parse().unwrap(), &fixture()).await.unwrap();
    let live = Live::bind(if cfg!(windows) { "python" } else { "python3" }, relay.observed.clone()).await.unwrap();
    World {
        relay: Some(relay),
        live: Some(live),
        ..World::default()
    }
}

#[tokio::test]
async fn t3_m3_cleanup_joins_only_its_own_listeners_and_is_idempotent() {
    let (mut first, mut retained) = tokio::join!(listening(), listening());
    let address = retained.relay.as_ref().unwrap().address;
    assert!(first.released.is_empty() && retained.released.is_empty());
    assert_eq!(first.close().await, Ok(()));
    assert_eq!(first.released, ["relay", "live"]);
    assert!(first.relay.is_none() && first.live.is_none());
    assert_eq!(retained.released, [] as [&str; 0]);
    // The other world's listener is still owned, so completed cleanup cannot
    // supply its release observation. Never connect to the closed world.
    let connection = tokio::net::TcpStream::connect(address).await.unwrap();
    drop(connection);
    assert_eq!(retained.close().await, Ok(()));
    assert_eq!(retained.released, ["relay", "live"]);
    assert_eq!(first.close().await, Ok(()));
    assert_eq!(first.released, ["relay", "live"]);
}

async fn child(code: u8) -> (Process, std::sync::mpsc::Receiver<std::process::ExitStatus>) {
    let mut command = tokio::process::Command::new("python3");
    command.args([
        "-u",
        "-c",
        &format!("import sys; print('{{\"event\":\"ready\"}}'); sys.stdin.read(); sys.exit({code})"),
    ]);
    let mut process = Process::start(&mut command).unwrap();
    process.expect("ready").await.unwrap();
    let collected = process.observe_collection();
    assert!(matches!(
        collected.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    (process, collected)
}

#[tokio::test]
async fn t3_m3_cleanup_collects_children_in_order_and_preserves_failure() {
    for code in [0, 7] {
        let mut world = listening().await;
        let (bridge, bridge_exit) = child(code).await;
        let (host, host_exit) = child(0).await;
        world.bridge = Some(bridge);
        world.host = Some(host);
        let root = tempfile::tempdir().unwrap();
        world.root = root.path().join("private");
        std::fs::create_dir(&world.root).unwrap();
        std::fs::write(world.root.join("bridge.private.jsonl"), "").unwrap();
        assert_eq!(world.released, [] as [&str; 0]);
        let result = world.close().await;
        assert_eq!(bridge_exit.try_recv().unwrap().code(), Some(i32::from(code)));
        assert!(host_exit.try_recv().unwrap().success());
        assert!(world.bridge.is_none() && world.host.is_none());
        assert!(world.relay.is_none() && world.live.is_none());
        assert!(!world.root.exists());
        if code == 0 {
            assert_eq!(result, Ok(()));
            assert_eq!(world.released, ["bridge", "relay", "live", "host"]);
        } else {
            assert_eq!(result, Err("cleanup: owned child exited nonzero".into()));
            assert_eq!(world.released, ["relay", "live", "host"]);
        }
    }
}
