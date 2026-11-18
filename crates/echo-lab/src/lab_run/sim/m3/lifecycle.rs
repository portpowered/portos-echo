//! T3: real task joins and child collection behind the CLI's `world_closed` check.
use super::*;

impl World {
    pub(super) fn record_release<T, E>(&mut self, owner: &'static str, result: &Result<T, E>) {
        if result.is_ok() {
            self.released.push(owner);
        }
    }
}

async fn listening() -> World {
    let relay = Relay::bind("127.0.0.1:1".parse().unwrap(), &fixture()).await.unwrap();
    let live = Live::bind(relay.observed.clone()).await.unwrap();
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
    let mut command = tokio::process::Command::new(if cfg!(windows) { "python" } else { "python3" });
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
