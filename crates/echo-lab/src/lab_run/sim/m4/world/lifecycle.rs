//! T3 lifecycle witnesses through prebuilt bridge and pinned host.
use super::super::super::process::owners::{Completion, Host};
use super::*;

fn root() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    std::env::temp_dir().join(format!(
        "m4-lifecycle-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

async fn start(world: &mut World, root: &Path) -> Owners {
    start_case(world, root, false).await
}

async fn start_case(world: &mut World, root: &Path, negative: bool) -> Owners {
    let host = Host::new();
    let bridge = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug")
        .join(if cfg!(windows) {
            "echo-bridge.exe"
        } else {
            "echo-bridge"
        });
    world.start(host.python(), &bridge, root, negative).await.unwrap();
    Owners {
        host,
        host_child: world.host.as_mut().unwrap().observe_collection(),
        bridge_child: world.bridge.as_mut().unwrap().observe_collection(),
    }
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host and prebuilt echo-bridge"]
async fn m6_t3_http500_never_animates_and_plays_honest_fallback_once() {
    let root = root();
    let mut world = World {
        emotion: true,
        ..World::default()
    };
    let owners = start_case(&mut world, &root, true).await;
    let (completion, timing) = world.speak().await.unwrap();
    let observer = world.observer().unwrap();
    observer.wait_for(0, |call| call.reply_completed).await.unwrap();
    super::super::tests::assert_dialogue(&observer, FALLBACK);
    assert_eq!(completion, json!({"event":"bridge.run_completed","run_id":"run-1"}));
    assert_eq!(timing.played, 9600);
    assert_eq!(close(&mut world).await, Ok(()));
    let states: Vec<_> = world.ui_states.iter().map(|f| f["state"].clone()).collect();
    let expected = super::super::emotion::sequence();
    assert_eq!(
        states,
        vec![
            expected[0].clone(),
            expected[1].clone(),
            expected[2].clone(),
            expected[5].clone(),
            expected[6].clone()
        ]
    );
    assert!(
        world
            .ui_states
            .windows(2)
            .all(|pair| pair[0]["seq"].as_u64() < pair[1]["seq"].as_u64())
    );
    let measurement = super::super::emotion::measurement(&world);
    assert_eq!(measurement["frame_count"], 0);
    assert_eq!(measurement["return_state"], json!({}));
    assert_eq!(measurement["host_pcm_bytes"], 9600);
    owners.assert_closed();
    assert!(!root.exists());
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host and prebuilt echo-bridge"]
async fn m6_t3_eof_during_animation_reaps_pending_expiry_model_and_all_listeners() {
    let root = root();
    let mut world = World {
        emotion: true,
        ..World::default()
    };
    let owners = start(&mut world, &root).await;
    let mut screen = Screen::connect(&root).await.unwrap();
    world.observe(&mut screen, 0, "ready").await.unwrap();
    world.control(json!({"action":"finite","pcm":fixture()})).await.unwrap();
    world.observe(&mut screen, 1, "listening").await.unwrap();
    world.observe(&mut screen, 2, "connected").await.unwrap();
    world.observe(&mut screen, 3, "emotion").await.unwrap();
    world.model.as_ref().unwrap().wait_for_result_request().await;
    let partial = super::super::emotion::measurement(&world);
    assert_eq!(partial["return_state"], json!({}));
    assert_eq!(partial["reply_pcm_bytes"], 0);
    assert_eq!(
        close(&mut world).await,
        Err("model: expected exactly two requests and verified tool output".into())
    );
    assert!(host.try_recv().unwrap().success());
    assert!(bridge.try_recv().unwrap().success());
    world.observe(&mut screen, 4, "connected").await.unwrap();
    let measurement = super::super::emotion::measurement(&world);
    assert_eq!(measurement["frame_count"], 2);
    assert_eq!(measurement["frame_ms"], 80);
    assert_eq!(measurement["return_state"], json!({"state":"connected"}));
    assert_eq!(measurement["reply_pcm_bytes"], 0);
    assert_eq!(screen.next().await, Err("screen: EOF before observation".into()));
    assert!(
        !world
            .trace
            .records
            .iter()
            .any(|e| e["event"] == "session.commentary.append")
    );
    owners.assert_closed();
    assert!(!root.exists());
    assert_eq!(world.close().await, Ok(()));
}

fn endpoints(world: &World) -> Vec<String> {
    let raw = std::fs::read_to_string(world.root.join("trace")).unwrap();
    let mut addresses: Vec<_> = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|v| v["address"].as_str().map(str::to_owned))
        .collect();
    // The single-accept relay releases its listener before bridge readiness;
    // its former address has no overlapping lifetime at this observation.
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

// The close API awaits the real relay, DialogueLive, Model and Spoof task
// joins even when another cleanup step fails. Retain that actual future;
// observing a taken Option or a closed connection would not prove completion.
async fn close(world: &mut World) -> Result<(), String> {
    let mut owned = std::mem::take(world);
    let mut completion = Completion::new("M4 bridge/relay/Live/model/spoof/host", async move {
        let result = owned.close().await;
        (owned, result)
    });
    completion.stop();
    let (owned, result) = completion.assert_closed().await;
    *world = owned;
    result
}

struct Owners {
    host: Host,
    host_child: std::sync::mpsc::Receiver<std::process::ExitStatus>,
    bridge_child: std::sync::mpsc::Receiver<std::process::ExitStatus>,
}

impl Owners {
    fn assert_closed(self) {
        assert!(
            self.host_child.try_recv().unwrap().success(),
            "M4 host child not collected"
        );
        assert!(
            self.bridge_child.try_recv().unwrap().success(),
            "M4 bridge child not collected"
        );
        self.host.assert_closed();
    }
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host and prebuilt echo-bridge"]
async fn t3_m4_cancel_active_reply_reaps_children_listeners_and_private_files() {
    let root = root();
    let mut world = World::default();
    let owners = start(&mut world, &root).await;
    let mut screen = Screen::connect(&root).await.unwrap();
    screen.expect("ready").await.unwrap();
    let cancel = root.join("cancel");
    let cancelled = async {
        screen.expect("listening").await.unwrap();
        screen.expect("connected").await.unwrap();
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
    assert_eq!(close(&mut world).await, Ok(()));
    assert!(!root.exists());
    owners.assert_closed();
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
        close(&mut world).await,
        Err("cleanup: private M4 directory removal failed".into())
    );
    assert_eq!(std::fs::read_to_string(&root).unwrap(), "replacement");
    std::fs::remove_file(&root).unwrap();
    assert_eq!(world.close().await, Ok(()));
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host and prebuilt echo-bridge"]
async fn t3_m4_parallel_world_failure_does_not_borrow_success_or_leave_listeners() {
    async fn case(mut world: World, root: PathBuf, owners: Owners, fail: bool) -> PathBuf {
        if fail {
            let live = world.live.take().unwrap();
            live.shutdown().await.unwrap();
            assert!(world.speak().await.is_err());
            assert!(world.speaking.is_none());
            assert!(close(&mut world).await.is_err());
        } else {
            assert_eq!(world.speak().await.unwrap().0["run_id"], "run-1");
            assert_eq!(close(&mut world).await, Ok(()));
        }
        assert!(!root.exists());
        owners.assert_closed();
        root
    }
    let (first_root, second_root) = (root(), root());
    let (mut first, mut second) = (World::default(), World::default());
    let (first_owners, second_owners) = tokio::join!(start(&mut first, &first_root), start(&mut second, &second_root));
    // Both worlds still own their endpoints at this comparison. Historical
    // addresses may be reused after either world releases them.
    let (a, b) = (endpoints(&first), endpoints(&second));
    assert!(a.iter().all(|address| !b.contains(address)));
    let (failed, passed) = tokio::join!(
        case(first, first_root, first_owners, true),
        case(second, second_root, second_owners, false),
    );
    assert_ne!(failed, passed);
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host and prebuilt echo-bridge"]
async fn t3_m4_teardown_failure_surfaces_after_collecting_all_peers() {
    let root = root();
    let retained = root.with_extension("retained");
    let mut world = World::default();
    let owners = start(&mut world, &root).await;
    assert_eq!(world.speak().await.unwrap().0["run_id"], "run-1");
    // Replace the owned destination after the bridge has closed its trace.
    // This deterministically exercises a real filesystem cleanup failure on
    // Windows and Linux, without changing permissions or relying on sleeps.
    world.close_bridge(&mut Vec::new()).await;
    std::fs::rename(&root, &retained).unwrap();
    std::fs::write(&root, "replacement").unwrap();
    assert_eq!(
        close(&mut world).await,
        Err("cleanup: private M4 directory removal failed".into())
    );
    owners.assert_closed();
    assert_eq!(std::fs::read_to_string(&root).unwrap(), "replacement");
    std::fs::remove_file(&root).unwrap();
    std::fs::remove_dir_all(retained).unwrap();
    assert_eq!(world.close().await, Ok(()));
}
