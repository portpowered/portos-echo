//! T3 lifecycle through prebuilt production binaries and pinned host.
use super::super::super::process::owners::Completion;
#[cfg(unix)]
use super::super::super::process::owners::Host;
use super::*;

// M7 runs Unix Muse services (muse::prebuilt refuses other platforms), so its T3 world tests
// and their helpers exist only on Unix.
#[cfg(unix)]
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

#[cfg(unix)]
async fn start(world: &mut World, root: &Path, bridge: &Path, host: &Host) -> Result<(), String> {
    world.start(host.python(), bridge, root, false, &binaries()).await
}

async fn close(world: &mut World) -> Result<(), String> {
    let mut owned = std::mem::take(world);
    let mut completion = Completion::new("M7 daemon/helper/bridge/relay/provider/spoof/host", async move {
        let result = owned.close().await;
        (owned, result)
    });
    completion.stop();
    let (owned, result) = completion.assert_closed().await;
    *world = owned;
    result
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "T3 requires Linux, openssl, pinned hassmic-host and prebuilt bridge/Muse binaries"]
async fn m7_partial_startup_reaps_daemon_helper_host_and_all_listeners() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("world");
    let host_owner = Host::new();
    let mut world = World::default();
    assert_eq!(
        start(&mut world, &root, &root.join("missing-bridge"), &host_owner).await,
        Err("launch: cannot spawn owned process".into())
    );
    let daemon = world.muse.daemon.as_mut().unwrap().observe_collection();
    let host = world.host.as_mut().unwrap().observe_collection();
    assert_eq!(close(&mut world).await, Ok(()));
    assert!(daemon.try_recv().unwrap().success());
    assert!(host.try_recv().unwrap().success());
    assert!(!root.exists());
    host_owner.assert_closed();
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "T3 requires Linux, openssl, pinned hassmic-host and prebuilt bridge/Muse binaries"]
async fn m7_cancel_pending_task_joins_all_owners_and_removes_private_world() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("world");
    let cancel = parent.path().join("cancel");
    let host_owner = Host::new();
    let mut world = World::default();
    start(&mut world, &root, &binaries().join("echo-bridge"), &host_owner)
        .await
        .unwrap();
    let daemon = world.muse.daemon.as_mut().unwrap().observe_collection();
    let bridge = world.bridge.as_mut().unwrap().observe_collection();
    let host = world.host.as_mut().unwrap().observe_collection();
    // Cancel after a genuine submitted task has emitted Progress, before report.
    let mut task = None;
    let journey = world.speak_with(async |root, _muse| {
        task = Some(progress(root).await?);
        std::fs::write(&cancel, "cancel").unwrap();
        std::future::pending::<Result<(), String>>().await
    });
    assert_eq!(
        super::super::super::controlled(
            journey,
            Some(&cancel),
            tokio::time::Instant::now() + std::time::Duration::from_secs(20)
        )
        .await,
        Err("sim: cancelled".into())
    );
    let mut failures = Vec::new();
    world.close_bridge(&mut failures).await;
    assert_eq!(failures, Vec::<String>::new());
    assert_cancelled_muse_task(&mut world, &task.unwrap()).await;
    assert_eq!(
        world.control(json!({"action":"playback"})).await.unwrap()["pcm_bytes"],
        0
    );
    assert_eq!(close(&mut world).await, Ok(()));
    for collected in [daemon, bridge, host] {
        assert!(collected.try_recv().unwrap().success());
    }
    assert!(!root.exists());
    assert_eq!(
        world
            .trace
            .records
            .iter()
            .filter(|e| e["event"] == "muse.terminal")
            .count(),
        0
    );
    host_owner.assert_closed();
    assert_eq!(close(&mut world).await, Ok(()));
}

#[cfg(unix)]
async fn assert_cancelled_muse_task(world: &mut World, id: &str) {
    let response = world.muse.report(id).await.unwrap();
    assert_eq!(response["exit"], 1);
    assert_eq!(response["stdout"], "");
    assert!(response["stderr"].as_str().unwrap().contains("cancelled task_id"));
    world.muse.collect().unwrap();
    let calls = &world.muse.calls;
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["argv"][2], calls[1]["argv"][2]);
    assert_eq!(
        calls[1]["argv"][5],
        format!("Cancel task {id}. Do not call echo.report.result for it.")
    );
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
    assert_eq!(close(&mut world).await, Ok(()));
}

#[tokio::test]
async fn m7_missing_daemon_owner_is_cleanup_failure() {
    let mut muse = super::super::muse::Muse::default();
    muse.url = "https://127.0.0.1:12345".into();
    assert_eq!(
        muse.close().await,
        Err("m7: missing daemon owner during cleanup".into())
    );
}

#[tokio::test]
#[ignore = "T3 starts a local child process"]
async fn m7_daemon_collection_failure_is_propagated() {
    let mut command = tokio::process::Command::new("python3");
    command.args([
        "-c",
        "import json,socket,threading,sys; listener=socket.socket(); listener.bind((sys.argv[1],0)); listener.listen(); print(json.dumps({'port':listener.getsockname()[1]}),flush=True); threading.Event().wait()",
        "127.0.0.1",
    ]);
    let mut muse = super::super::muse::Muse::default();
    muse.daemon = Some(super::super::super::process::Process::start(&mut command).unwrap());
    let ready = muse.daemon.as_mut().unwrap().line().await.unwrap();
    let daemon = muse.daemon.as_mut().unwrap().observe_collection();
    let origin = "https://127.0.0.1";
    muse.url = format!("{origin}:{}", ready["port"]);
    assert_eq!(muse.close().await, Err("cleanup: owned child exited nonzero".into()));
    assert!(!daemon.try_recv().unwrap().success());
    assert_eq!(muse.close().await, Ok(()));
}
