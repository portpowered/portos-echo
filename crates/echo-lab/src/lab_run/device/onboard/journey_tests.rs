//! T3: the runtime's own launch, wake and recovery steps against the synthetic Echo
//! and a loopback screen stream owned by the test.
use super::{PRIVATE, UNLINKED, World, remote, world};
use crate::lab_run::{device::runtime::Runtime, run_report::Artifacts};
use serde_json::{Value, json};
use std::{cell::RefCell, net::Ipv4Addr, rc::Rc};
use tokio::io::AsyncWriteExt;

const DERIVED: &str = "# derived for this run\n";

fn runtime(world: &Rc<RefCell<World>>) -> Runtime {
    let mut state = Runtime::default();
    state.remote = Some(remote(world));
    state
}

/// Serves the first presentation to every connection, as the bridge's screen stream does.
async fn screen(world: &Rc<RefCell<World>>) {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    world.borrow_mut().forward = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((mut socket, _)) = listener.accept().await {
            let frame = json!({"seq":1,"state":{"state":"ready","message":"Echo Live ready"}});
            let response = format!("HTTP/1.1 200 OK\r\n\r\ndata: {frame}\n");
            socket.write_all(response.as_bytes()).await.unwrap();
            held.push(socket);
        }
    });
}

async fn started() -> (tempfile::TempDir, Rc<RefCell<World>>, Runtime) {
    let root = tempfile::tempdir().unwrap();
    let world = world();
    screen(&world).await;
    let mut state = runtime(&world);
    state.start(None, DERIVED, root.path(), None).await.unwrap();
    (root, world, state)
}

fn wake(world: &Rc<RefCell<World>>, run: &str, frames: u64, at: u64) {
    let records = [
        json!({"event":"wyoming.run_started","run_id":run,"elapsed_ms":at}),
        json!({"event":"wyoming.mic_frames","run_id":run,"elapsed_ms":at + 1,"count":frames}),
        json!({"event":"reply.played","run_id":run,"elapsed_ms":at + 2,"outcome":"played"}),
        json!({"event":"wyoming.run_ended","run_id":run,"elapsed_ms":at + 3}),
        json!({"event":"bridge.ready","elapsed_ms":at + 4}),
        json!({"event":"bridge.run_completed","run_id":run,"elapsed_ms":at + 5}),
    ];
    world
        .borrow_mut()
        .trace
        .push_str(&records.map(|record| format!("{record}\n")).concat());
}

#[tokio::test]
async fn t3_on_device_launch_pushes_the_derived_config_and_starts_in_its_own_session() {
    let (root, world, state) = started().await;
    let staged = root.path().join("configured.private/bridge.toml");
    assert_eq!((state.generation, state.bridge_id()), (1, Ok(Some(1))));
    let world = world.borrow();
    assert_eq!(
        world.pushed,
        [(
            staged.to_string_lossy().into_owned(),
            format!("{PRIVATE}/bridge.toml"),
            DERIVED.to_owned()
        )]
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&staged).unwrap().permissions().mode() & 0o777, 0o600);
    }
    for expected in [
        format!("runas root 3990,inet chmod 600 {PRIVATE}/bridge.toml"),
        format!("runas root 3990,inet stat -c %a {PRIVATE}/bridge.toml"),
        format!("runas root 3990,inet sh -c 'umask 077; cd {PRIVATE} || exit 1;"),
        "setsid sh -c".into(),
        format!("--config {PRIVATE}/bridge.toml --trace {PRIVATE}/trace.jsonl <lab-stdin"),
        format!("exec-out runas root 3990,inet cat {PRIVATE}/trace.jsonl"),
        "tail -c +1 /data/local/hassmic/boot.log".into(),
        "forward tcp:0 tcp:16900".into(), // lint-rules: allow-literal synthetic adb argument comparison, no connection
    ] {
        assert!(world.ran(&expected), "{expected}");
    }
    assert!(!world.ran("pgrep"));
}

#[tokio::test]
async fn t3_on_device_m1_wake_is_judged_from_the_trace_read_back_and_cleanup_is_owned() {
    let (root, world, mut state) = started().await;
    state.before_wake().await.unwrap();
    state.arm_wake().unwrap();
    wake(&world, "run-1", 12, 10);
    state.complete(3).await.unwrap();
    state.wake_linked().unwrap();
    assert_eq!(
        state.wakes,
        [json!({"bridge_generation":1,"run_id":"run-1","mic_frames":12})]
    );
    let mut artifacts = Artifacts::create(root.path().join("report")).unwrap();
    state.close(root.path(), &mut artifacts).await.unwrap();
    assert!(artifacts.checks["bridge_errors_absent"]);
    let world = world.borrow();
    assert!(!world.alive && !root.path().join("configured.private").exists());
    let forward = format!("forward --remove tcp:{}", world.forward);
    let private = format!("rm -f {PRIVATE}/bridge.toml {PRIVATE}/trace.jsonl");
    assert!(world.ran(&forward) && world.ran(&private));
}

#[tokio::test]
async fn t3_on_device_m1_wake_below_its_frame_floor_fails_with_the_observed_count() {
    let (_root, world, mut state) = started().await;
    state.arm_wake().unwrap();
    wake(&world, "run-1", 4, 10);
    let error = state.complete(3).await.unwrap_err();
    assert!(error.contains("wyoming.mic_frames"), "{error}");
    assert!(error.contains("expected 1 occurrences, observed 0"), "{error}");
    assert_eq!(state.wakes, Vec::<Value>::new());
    world.borrow_mut().alive = false;
    assert_eq!(
        state.complete(4).await.unwrap_err(),
        "process: exited before observation"
    );
}

#[tokio::test]
async fn t3_on_device_pushed_config_must_read_back_owner_only() {
    let root = tempfile::tempdir().unwrap();
    let world = world();
    world.borrow_mut().mode = "644".into();
    let mut state = runtime(&world);
    assert_eq!(
        state.start(None, DERIVED, root.path(), None).await.unwrap_err(),
        "device: unmet prerequisite: pushed bridge configuration with mode 0600 on the Echo"
    );
    assert!(!world.borrow().ran("setsid"));
}

#[tokio::test(start_paused = true)]
async fn t3_on_device_launch_and_wake_without_a_hassmic_link_fail_by_name() {
    let root = tempfile::tempdir().unwrap();
    let world = world();
    world.borrow_mut().links = false;
    // A connection logged before this launch proves nothing about this bridge.
    world.borrow_mut().log = "hassmic: client connected\n".into();
    let mut state = runtime(&world);
    let launched = state.start(None, DERIVED, root.path(), None).await;
    assert_eq!(state.startup_result(launched).unwrap_err(), UNLINKED);

    let world = super::world();
    world.borrow_mut().log = "client connected\nsatellite: running\nclient disconnected\n".into();
    let mut state = runtime(&world);
    assert_eq!(state.before_wake().await.unwrap_err(), UNLINKED);
    world.borrow_mut().log.push_str("satellite: running\n");
    state.before_wake().await.unwrap();
    state.wake_linked().unwrap();
    world
        .borrow_mut()
        .log
        .push_str("wake word: no Home Assistant link, nothing started\n");
    assert_eq!(
        state.wake_linked().unwrap_err(),
        "device: wake without a hassmic link; hassmic logged \"wake word: no Home Assistant link, nothing started\""
    );
}

#[tokio::test]
async fn t3_on_device_hassmic_restart_relaunches_a_bridge_that_ended_with_its_link() {
    let (root, world, mut state) = started().await;
    let mut artifacts = Artifacts::create(root.path().join("report")).unwrap();
    let offset = state.arm_relink().unwrap();
    world
        .borrow_mut()
        .log
        .push_str("client disconnected\nclient connected\n");
    state.relinked(offset, &mut artifacts).await.unwrap();
    assert_eq!((state.generation, state.bridge_id()), (1, Ok(Some(1))));

    let offset = state.arm_relink().unwrap();
    {
        let mut world = world.borrow_mut();
        world.alive = false;
        world.log.push_str("client disconnected\n");
        let error = json!({"event":"bridge.error","elapsed_ms":9,"reason":"link lost"});
        let line = format!("{error}\n");
        world.trace.push_str(&line);
    }
    state.relinked(offset, &mut artifacts).await.unwrap();
    assert_eq!((state.generation, state.bridge_id()), (2, Ok(Some(2))));
    state.close(root.path(), &mut artifacts).await.unwrap();
    assert!(artifacts.checks["bridge_errors_absent"]);
}
