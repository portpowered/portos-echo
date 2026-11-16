//! T3 lifecycle through prebuilt production binaries and pinned host.
use super::*;

fn binaries() -> PathBuf {
    // Cargo places tests under <profile>/deps or <profile>/build/<crate>/<hash>/out.
    // Follow the executable so llvm-cov's separate target uses its own artifacts.
    let executable = std::env::current_exe().unwrap();
    let directory = executable
        .ancestors()
        .find(|path| matches!(path.file_name().and_then(|name| name.to_str()), Some("deps" | "build")))
        .and_then(Path::parent)
        .unwrap()
        .to_owned();
    for name in [
        "echo-bridge",
        "echo-muse-daemon",
        "echo-muse-report",
        "echo-muse-fake-gadget",
    ] {
        assert!(
            directory.join(name).is_file(),
            "missing prebuilt {name} in {}",
            directory.display()
        );
    }
    directory
}

async fn start(world: &mut World, root: &Path, bridge: &Path) -> Result<(), String> {
    world.start("python3", bridge, root, false, &binaries()).await
}

fn endpoints(world: &World) -> Vec<String> {
    let mut addresses = vec![
        world.muse.url.trim_start_matches("https://").into(),
        world.relay.as_ref().unwrap().address.to_string(),
    ];
    for origin in [
        world.model.as_ref().unwrap().origin.as_str(),
        world.live.as_ref().unwrap().base_url(),
        world.spoof.as_ref().unwrap().url(),
    ] {
        addresses.push(origin.trim_start_matches("http://").split('/').next().unwrap().into());
    }
    addresses
}

async fn closed(addresses: Vec<String>) {
    for address in addresses {
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }
}

#[tokio::test]
#[ignore = "T3 requires Linux, openssl, pinned hassmic-host and prebuilt bridge/Muse binaries"]
async fn m7_partial_startup_reaps_daemon_helper_host_and_all_listeners() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("world");
    let mut world = World::default();
    assert_eq!(
        start(&mut world, &root, &root.join("missing-bridge")).await,
        Err("launch: cannot spawn owned process".into())
    );
    let addresses = endpoints(&world);
    let daemon = world.muse.daemon.as_mut().unwrap().observe_collection();
    let host = world.host.as_mut().unwrap().observe_collection();
    assert_eq!(world.close().await, Ok(()));
    assert!(daemon.try_recv().unwrap().success());
    assert!(host.try_recv().unwrap().success());
    assert!(!root.exists());
    closed(addresses).await;
}

#[tokio::test]
#[ignore = "T3 requires Linux, openssl, pinned hassmic-host and prebuilt bridge/Muse binaries"]
async fn m7_cancel_pending_task_joins_all_owners_and_removes_private_world() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("world");
    let cancel = parent.path().join("cancel");
    let mut world = World::default();
    start(&mut world, &root, &binaries().join("echo-bridge")).await.unwrap();
    let addresses = endpoints(&world);
    let daemon = world.muse.daemon.as_mut().unwrap().observe_collection();
    let bridge = world.bridge.as_mut().unwrap().observe_collection();
    let host = world.host.as_mut().unwrap().observe_collection();
    // Cancel after a genuine submitted task has emitted Progress, before report.
    let journey = async {
        world.control(json!({"action":"finite","pcm":fixture()})).await?;
        world.progress().await?;
        std::fs::write(&cancel, "cancel").unwrap();
        std::future::pending::<Result<(), String>>().await
    };
    assert_eq!(
        super::super::super::controlled(
            journey,
            Some(&cancel),
            tokio::time::Instant::now() + std::time::Duration::from_secs(20)
        )
        .await,
        Err("sim: cancelled".into())
    );
    assert_eq!(world.close().await, Ok(()));
    for collected in [daemon, bridge, host] {
        assert!(collected.try_recv().unwrap().success());
    }
    assert!(!root.exists());
    assert_eq!(
        world
            .trace
            .records
            .iter()
            .filter(|e| e["event"] == "session.commentary.append")
            .count(),
        0
    );
    closed(addresses).await;
    assert_eq!(world.close().await, Ok(()));
}

#[tokio::test]
async fn m7_private_cleanup_failure_is_propagated() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("world");
    std::fs::write(&root, "replacement").unwrap();
    let mut world = World {
        root: root.clone(),
        ..World::default()
    };
    assert!(
        world
            .close()
            .await
            .unwrap_err()
            .starts_with("m7: private world removal failed")
    );
    assert_eq!(std::fs::read(&root).unwrap(), b"replacement");
    std::fs::remove_file(root).unwrap();
    assert_eq!(world.close().await, Ok(()));
}
