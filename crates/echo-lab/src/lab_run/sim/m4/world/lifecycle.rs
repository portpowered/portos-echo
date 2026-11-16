//! T3 lifecycle witnesses through prebuilt bridge and pinned host.
use super::*;
use tokio::net::TcpStream;

fn root() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    std::env::temp_dir().join(format!(
        "m4-lifecycle-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

async fn start(world: &mut World, root: &Path) {
    let bridge = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug")
        .join(if cfg!(windows) {
            "echo-bridge.exe"
        } else {
            "echo-bridge"
        });
    world
        .start(if cfg!(windows) { "python" } else { "python3" }, &bridge, root, false)
        .await
        .unwrap();
}

fn endpoints(world: &World) -> Vec<String> {
    let raw = std::fs::read_to_string(world.root.join("trace")).unwrap();
    let mut addresses: Vec<_> = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|v| v["address"].as_str().map(str::to_owned))
        .collect();
    addresses.push(world.relay.as_ref().unwrap().address.to_string());
    for origin in [
        world.model.as_ref().unwrap().origin.as_str(),
        world.live.as_ref().unwrap().base_url(),
        world.spoof.as_ref().unwrap().url(),
    ] {
        addresses.push(
            origin
                .strip_prefix("http://")
                .unwrap()
                .split('/')
                .next()
                .unwrap()
                .to_owned(),
        );
    }
    addresses
}

async fn closed(addresses: &[String]) {
    for address in addresses {
        assert!(
            TcpStream::connect(address).await.is_err(),
            "listener survived: {address}"
        );
    }
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host and prebuilt echo-bridge"]
async fn t3_m4_cancel_active_reply_reaps_children_listeners_and_private_files() {
    let root = root();
    let mut world = World::default();
    start(&mut world, &root).await;
    let addresses = endpoints(&world);
    let host = world.host.as_mut().unwrap().observe_collection();
    let bridge = world.bridge.as_mut().unwrap().observe_collection();
    let mut screen = Screen::connect(&root).await.unwrap();
    screen.expect("ready").await.unwrap();
    let cancel = root.join("cancel");
    let cancelled = async {
        screen.expect("listening").await.unwrap();
        screen.expect("speaking").await.unwrap();
        std::fs::write(&cancel, "cancel").unwrap();
    };
    let (result, ()) = tokio::join!(
        super::super::super::controlled(
            world.speak(),
            Some(&cancel),
            tokio::time::Instant::now() + std::time::Duration::from_secs(20)
        ),
        cancelled,
    );
    assert_eq!(result.err(), Some("sim: cancelled".into()));
    assert_eq!(world.close().await, Ok(()));
    assert!(host.try_recv().unwrap().success());
    assert!(bridge.try_recv().unwrap().success());
    assert!(!root.exists());
    closed(&addresses).await;
    assert_eq!(world.close().await, Ok(()));
}

#[tokio::test]
async fn cleanup_replaced_directory_is_reported_and_other_owners_still_close() {
    let root = root();
    std::fs::write(&root, "replacement").unwrap();
    let mut world = World {
        root: root.clone(),
        ..World::default()
    };
    assert_eq!(
        world.close().await,
        Err("cleanup: private M4 directory removal failed".into())
    );
    assert_eq!(std::fs::read_to_string(&root).unwrap(), "replacement");
    std::fs::remove_file(&root).unwrap();
    assert_eq!(world.close().await, Ok(()));
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host and prebuilt echo-bridge"]
async fn t3_m4_parallel_world_failure_does_not_borrow_success_or_leave_listeners() {
    async fn case(fail: bool) -> (PathBuf, Vec<String>) {
        let root = root();
        let mut world = World::default();
        start(&mut world, &root).await;
        let addresses = endpoints(&world);
        if fail {
            let live = world.live.take().unwrap();
            live.shutdown().await.unwrap();
            assert!(world.speak().await.is_err());
            assert!(world.speaking.is_none());
            assert!(world.close().await.is_err());
        } else {
            assert_eq!(world.speak().await.unwrap().0["run_id"], "run-1");
            assert_eq!(world.close().await, Ok(()));
        }
        assert!(!root.exists());
        closed(&addresses).await;
        (root, addresses)
    }
    let (failed, passed) = tokio::join!(case(true), case(false));
    assert_ne!(failed.0, passed.0);
    assert!(failed.1.iter().all(|a| !passed.1.contains(a)));
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host and prebuilt echo-bridge"]
async fn t3_m4_teardown_failure_surfaces_after_collecting_all_peers() {
    let root = root();
    let retained = root.with_extension("retained");
    let mut world = World::default();
    start(&mut world, &root).await;
    let addresses = endpoints(&world);
    let host = world.host.as_mut().unwrap().observe_collection();
    let bridge = world.bridge.as_mut().unwrap().observe_collection();
    assert_eq!(world.speak().await.unwrap().0["run_id"], "run-1");
    // Replace the owned destination after the bridge has closed its trace.
    // This deterministically exercises a real filesystem cleanup failure on
    // Windows and Linux, without changing permissions or relying on sleeps.
    world.close_bridge(&mut Vec::new()).await;
    std::fs::rename(&root, &retained).unwrap();
    std::fs::write(&root, "replacement").unwrap();
    assert_eq!(
        world.close().await,
        Err("cleanup: private M4 directory removal failed".into())
    );
    assert!(host.try_recv().unwrap().success());
    assert!(bridge.try_recv().unwrap().success());
    closed(&addresses).await;
    assert_eq!(std::fs::read_to_string(&root).unwrap(), "replacement");
    std::fs::remove_file(&root).unwrap();
    std::fs::remove_dir_all(retained).unwrap();
    assert_eq!(world.close().await, Ok(()));
}
