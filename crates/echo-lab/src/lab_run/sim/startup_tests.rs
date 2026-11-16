//! T3 partial-startup fault at the Live factory boundary, with real acquired
//! host/Spoof handles. No fake substitutes for the resources being released.
use super::world::World;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
#[ignore = "T3 requires prebuilt pinned hassmic-host"]
async fn t3_failed_live_start_releases_the_already_running_host_and_spoof() {
    let root = std::env::temp_dir().join(format!("lab-partial-start-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let python = if cfg!(windows) { "python" } else { "python3" };
    let mut world = World::default();
    let result = world
        .start_with(python, std::path::Path::new("not-launched"), &root, async {
            Err(echo_live::call::CallError::Signaling)
        })
        .await;
    assert_eq!(result, Err("launch: fake Live startup failed".into()));
    assert_eq!(world.endpoints.len(), 2);
    for endpoint in &world.endpoints {
        let connection = tokio::net::TcpStream::connect(endpoint).await.unwrap();
        assert_eq!(connection.peer_addr().unwrap(), *endpoint);
        assert_eq!(endpoint.ip(), std::net::Ipv4Addr::LOCALHOST);
    }
    assert_eq!(world.close().await, Ok(()));
    assert_eq!(world.close().await, Ok(()));
    assert!(tokio::net::TcpStream::connect(world.endpoints[1]).await.is_err());
    if let Ok(mut stream) = tokio::net::TcpStream::connect(world.endpoints[0]).await {
        assert!(cfg!(windows) && stream.peer_addr().unwrap() == world.endpoints[0]);
        let _ = stream.write_all(b"{\"type\":\"describe\"}\n").await;
        let response = tokio::time::timeout(std::time::Duration::from_secs(1), stream.read_u8())
            .await
            .unwrap();
        assert!(response.is_err());
    }
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    std::fs::remove_dir(root).unwrap();
}

// T3 real child ownership checks; local stdin/pipe readiness, no sockets/sleeps.
#[test]
fn t3_drop_collects_after_runtime_teardown_and_dropped_close() {
    for dropped_close in [false, true] {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (process, collected) = runtime.block_on(async {
            let mut command = tokio::process::Command::new(if cfg!(windows) { "python" } else { "python3" });
            command.args([
                "-u",
                "-c",
                "import sys; print('{\"event\":\"ready\"}', flush=True); sys.stdin.buffer.read()",
            ]);
            let mut process = super::process::Process::start(&mut command).unwrap();
            process.expect("ready").await.unwrap();
            let collected = process.observe_collection();
            if dropped_close {
                let mut close = Box::pin(process.close());
                std::future::poll_fn(|cx| {
                    let _ = std::future::Future::poll(close.as_mut(), cx);
                    std::task::Poll::Ready(())
                })
                .await;
                drop(close);
            }
            (process, collected)
        });
        if dropped_close {
            // Drop on the runtime thread must not nest block_on or detach work.
            runtime.block_on(async {
                drop(process);
            });
            drop(runtime);
        } else {
            drop(runtime);
            drop(process);
        }
        let status = collected.try_recv().unwrap();
        assert_eq!(status.code(), Some(0));
    }
}

#[tokio::test]
async fn t3_close_collects_nonzero_and_is_idempotent() {
    let mut command = tokio::process::Command::new(if cfg!(windows) { "python" } else { "python3" });
    command.args([
        "-u",
        "-c",
        "import sys; print('{\"event\":\"ready\"}', flush=True); sys.stdin.buffer.read(); sys.exit(7)",
    ]);
    let mut process = super::process::Process::start(&mut command).unwrap();
    process.expect("ready").await.unwrap();
    let collected = process.observe_collection();
    assert_eq!(process.close().await, Err("cleanup: owned child exited nonzero".into()));
    assert_eq!(process.close().await, Err("cleanup: owned child exited nonzero".into()));
    drop(process);
    assert_eq!(collected.try_recv().unwrap().code(), Some(7));
}

#[tokio::test]
async fn t3_grace_waits_for_real_exit_after_eof_acknowledgement() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut command = tokio::process::Command::new(if cfg!(windows) { "python" } else { "python3" });
    command.args(["-u", "-c", "import socket,sys; s=socket.create_connection(('127.0.0.1',int(sys.argv[1]))); sys.stdin.buffer.read(); s.sendall(b'E'); assert s.recv(1)==b'R'", &listener.local_addr().unwrap().port().to_string()]);
    let mut process = super::process::Process::start(&mut command).unwrap();
    let collected = process.observe_collection();
    let (mut control, _) = listener.accept().await.unwrap();
    let mut close = Box::pin(process.close());
    tokio::select! {
        result = &mut close => panic!("returned before released exit: {result:?}"),
        value = control.read_u8() => assert_eq!(value.unwrap(), b'E'),
    }
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(close.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    control.write_all(b"R").await.unwrap();
    assert_eq!(close.await, Ok(()));
    assert_eq!(process.close().await, Ok(()));
    drop(process);
    assert_eq!(collected.try_recv().unwrap().code(), Some(0));
}

#[tokio::test]
async fn t3_cancelled_stop_keeps_its_deadline_and_collects_forced_exit() {
    let mut command = tokio::process::Command::new(if cfg!(windows) { "python" } else { "python3" });
    command.args([
        "-u",
        "-c",
        "import threading; print('{\"event\":\"ready\"}',flush=True); threading.Event().wait()",
    ]);
    let mut process = super::process::Process::start(&mut command).unwrap();
    process.expect("ready").await.unwrap();
    let collected = process.observe_collection();
    tokio::time::pause();
    for elapsed in [0, 4999] {
        tokio::time::advance(std::time::Duration::from_millis(elapsed)).await;
        let mut close = Box::pin(process.close());
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(close.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        drop(close);
    }
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    assert_eq!(process.close().await, Err("cleanup: owned child exited nonzero".into()));
    drop(process);
    let status = collected.try_recv().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(9));
    }
    #[cfg(windows)]
    assert_eq!(status.code(), Some(1));
}
