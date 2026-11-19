//! T3: configured recovery and delivered CLI replacement through paired HTTP.
use super::*;
use echo_bridge::bridge_voice::RuntimeCompletion;

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn t3_settings_configured_recovery_keeps_pending_edits_and_old_active_config() {
    let fixture = Fixture::new("chatgpt");
    let startup = settings_startup(&fixture);
    let home = startup.config.home.as_ref().unwrap().dir.clone();
    let path = fixture.directory.path().join("runtime-trace");
    let mut configured = Configured::from_effective(startup, path.clone(), ([127, 0, 0, 1], 0).into()).unwrap();
    let first = generation(&fixture, &mut configured, None).await;
    assert!(first.restart_requested);
    std::fs::remove_file(fixture.home.join("peer-port")).unwrap();
    let mut host = first.settings.unwrap();
    configured.replacement_failed(&mut host).unwrap();
    // Recovery must not reopen any of these as a new startup configuration.
    std::fs::write(fixture.directory.path().join("bridge.toml"), "invalid").unwrap();
    std::fs::write(home.join("settings.toml"), "invalid").unwrap();
    std::fs::write(home.join("prompts/voice.md"), " \n").unwrap();
    let second = generation(&fixture, &mut configured, Some(host)).await;
    assert!(!second.restart_requested);
    let mut host = second.settings.unwrap();
    retained_settings(&mut host, true);
    let rows = trace(&path);
    for (event, count) in [
        ("bridge.ready", 2),
        ("settings.restart_prepared", 1),
        ("settings.restart_failed", 1),
        ("settings.restart_recovered", 1),
        ("settings.overlay_ignored", 0),
    ] {
        assert_eq!(rows.iter().filter(|row| row["event"] == event).count(), count);
    }
    assert_eq!(std::fs::read_to_string(home.join("settings.toml")).unwrap(), "invalid");
    assert_eq!(std::fs::read_to_string(home.join("prompts/voice.md")).unwrap(), " \n");
}

async fn generation(
    fixture: &Fixture,
    configured: &mut Configured,
    retained: Option<Box<echo_bridge::bridge_settings::SettingsHost>>,
) -> RuntimeCompletion {
    let recovering = retained.is_some();
    let observer = Observer::default();
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let work = configured.execute_generation(
        FakeConnector::new(observer.clone()),
        async || -> Result<echo_media::Peer, Error> { unreachable!("idle creates no peer") },
        vec![],
        async { stopping.await.unwrap() },
        retained,
    );
    let inspect = inspect(fixture, recovering, stop);
    let (result, ()) = Box::pin(tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        tokio::join!(work, inspect)
    }))
    .await
    .unwrap();
    assert_eq!(
        observer.commands(),
        [Command::Connect, Command::Capture, Command::Probe, Command::Close]
    );
    super::super::host::released(fixture).await;
    result.unwrap()
}

async fn inspect(fixture: &Fixture, recovering: bool, stop: tokio::sync::oneshot::Sender<()>) {
    let _control = fixture.control().await;
    let mut stream = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
    let expected = if recovering {
        json!({"state":"error","relink":false,"message":"Could not restart bridge; saved settings remain pending"})
    } else {
        json!({"state":"ready","message":"Echo Live ready"})
    };
    while bridge_wire_screen::state(&mut stream).await != expected {}
    if recovering {
        recovered_surface(fixture).await;
        stop.send(()).unwrap();
    } else {
        let home = fixture.directory.path().join("agent-home");
        settings_runtime_requests(fixture, &home).await;
        assert_eq!(
            settings_runtime_patch(fixture, json!({"agent":{"max_tool_calls":3}})).await,
            json!({"version":1,"restart_pending":true})
        );
        assert_eq!(
            bridge_wire_screen::state(&mut stream).await,
            json!({"state":"ready","message":"Restarting bridge..."})
        );
    }
}

async fn recovered_surface(fixture: &Fixture) {
    let value: Value = reqwest::Client::new()
        .get(settings_runtime_url(fixture, "/settings/v1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(value["restart_pending"], true);
    assert_eq!(value["settings"]["agent"]["max_tool_calls"], 3);
    assert_eq!(value["prompts"]["voice"], "saved voice\n");
    assert_eq!(value["prompts"]["runner"], "admitted runner");
    assert_eq!(
        value["warnings"],
        json!(["Could not restart bridge; saved settings remain pending"])
    );
}

#[tokio::test]
#[ignore = "T3 requires prebuilt Linux echo-bridge and local Python"]
async fn t3_settings_binary_idle_save_execs_same_image_argv_and_held_stdin_once() {
    let fixture = Fixture::new("chatgpt");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    super::super::voice_entry::configure(&fixture, listener.local_addr().unwrap().port());
    let base = fixture.directory.path().join("bridge.toml");
    let home = fixture.directory.path().join("agent-home");
    let text = format!(
        "{}\n[home]\ndir='{}'\n",
        std::fs::read_to_string(&base).unwrap(),
        home.display()
    );
    std::fs::write(&base, &text).unwrap();
    let mut process = Process::spawn(&mut super::super::voice_entry::command(&fixture));
    let pid = process.child.id().unwrap();
    let original = process_identity(pid);
    let mut satellite = binary_ready(&listener, &mut process).await;
    let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
    assert_eq!(
        bridge_wire_screen::state(&mut screen).await["message"],
        "Echo Live ready"
    );
    assert_eq!(binary_snapshot(&fixture).await["restart_pending"], false);
    assert_eq!(
        settings_runtime_patch(&fixture, json!({"voice":{"session_listen_seconds":4}})).await,
        json!({"version":1,"restart_pending":true})
    );
    assert_eq!(
        bridge_wire_screen::state(&mut screen).await["message"],
        "Restarting bridge..."
    );
    let replacement = binary_ready(&listener, &mut process).await;
    assert_eq!(process.child.id(), Some(pid));
    assert_eq!(process_identity(pid), original);
    let mut remainder = vec![];
    tokio::time::timeout(OBSERVATION_TIMEOUT, satellite.read_to_end(&mut remainder))
        .await
        .unwrap()
        .unwrap();
    let value = binary_snapshot(&fixture).await;
    assert_eq!(value["restart_pending"], false);
    assert_eq!(value["settings"]["voice"]["session_listen_seconds"], 4);
    assert_eq!(std::fs::read_to_string(&base).unwrap(), text);
    let saved = home.join("settings.toml");
    assert_eq!(
        std::fs::read_to_string(&saved).unwrap(),
        "[voice]\nsession_listen_seconds = 4\n"
    );
    owner_only(&saved);
    binary_finished(&fixture, &mut process).await;
    drop(replacement);
}

async fn binary_finished(fixture: &Fixture, process: &mut Process) {
    drop(process.input.take());
    process.collect(0).await;
    assert_eq!(process.stderr().await, "");
    super::super::host::released(fixture).await;
    let rows = trace(&fixture.directory.path().join("trace"));
    assert_eq!(rows.iter().filter(|row| row["event"] == "bridge.ready").count(), 2);
    assert_eq!(
        rows.iter()
            .filter(|row| row["event"] == "settings.restart_prepared")
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("launches")).unwrap(),
        "started\nstarted\n"
    );
}

pub(super) fn process_identity(pid: u32) -> (PathBuf, Vec<u8>, PathBuf) {
    let root = PathBuf::from(format!("/proc/{pid}"));
    (
        std::fs::read_link(root.join("exe")).unwrap(),
        std::fs::read(root.join("cmdline")).unwrap(),
        std::fs::read_link(root.join("fd/0")).unwrap(),
    )
}

fn owner_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}

pub(super) async fn binary_ready(listener: &tokio::net::TcpListener, process: &mut Process) -> tokio::net::TcpStream {
    let mut satellite = tokio::time::timeout(OBSERVATION_TIMEOUT, listener.accept())
        .await
        .unwrap()
        .unwrap()
        .0;
    assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "run-satellite");
    assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "describe");
    bridge_short::send(&mut satellite, "info", json!({"satellite":{"name":"synthetic"}})).await;
    assert_eq!(process.line().await, r#"{"event":"bridge.ready"}"#);
    satellite
}

pub(super) async fn binary_snapshot(fixture: &Fixture) -> Value {
    reqwest::Client::new()
        .get(settings_runtime_url(fixture, "/settings/v1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

// Remove execute permission only from this case's private copy, after the OS
// has admitted it. /proc/self/exe then reaches the real failing exec syscall.
#[cfg(unix)]
#[tokio::test]
#[ignore = "T3 requires prebuilt Linux echo-bridge and local Python"]
async fn t3_settings_binary_failed_exec_remounts_pending_surface_without_retry() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("chatgpt");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    binary_home(&fixture, listener.local_addr().unwrap().port());
    let executable = fixture.directory.path().join("echo-bridge");
    let mut command = private_image(&fixture, &executable);
    let mut process = Process::spawn(&mut command);
    let pid = process.child.id().unwrap();
    let original = process_identity(pid);
    let satellite = binary_ready(&listener, &mut process).await;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        settings_runtime_patch(&fixture, json!({"voice":{"session_listen_seconds":4}})).await,
        json!({"version":1,"restart_pending":true})
    );
    let recovery = binary_ready(&listener, &mut process).await;
    assert_eq!(process_identity(pid), original);
    let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
    assert_eq!(
        bridge_wire_screen::state(&mut screen).await,
        json!({"state":"error","relink":false,
            "message":"Could not restart bridge; saved settings remain pending"})
    );
    binary_pending_surface(&fixture).await;
    binary_failed_trace(&fixture);
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let replacement = binary_retry(&fixture, &listener, &mut process).await;
    assert_eq!(process_identity(pid), original);
    drop(process.input.take());
    process.collect(0).await;
    assert_eq!(process.stderr().await, "");
    super::super::host::released(&fixture).await;
    binary_retry_trace(&fixture);
    drop((satellite, recovery, replacement));
}

async fn binary_retry(
    fixture: &Fixture,
    listener: &tokio::net::TcpListener,
    process: &mut Process,
) -> tokio::net::TcpStream {
    assert_eq!(
        settings_runtime_patch(fixture, json!({"agent":{"max_tool_calls":3}})).await,
        json!({"version":1,"restart_pending":true})
    );
    let replacement = binary_ready(listener, process).await;
    let snapshot = binary_snapshot(fixture).await;
    assert_eq!(snapshot["restart_pending"], false);
    assert_eq!(snapshot["warnings"], json!([]));
    assert_eq!(snapshot["settings"]["voice"]["session_listen_seconds"], 4);
    assert_eq!(snapshot["settings"]["agent"]["max_tool_calls"], 3);
    assert_eq!(snapshot["prompts"]["voice"], "accepted after failed exec\n");
    let path = fixture.directory.path().join("agent-home/settings.toml");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[agent]\nmax_tool_calls = 3\n\n[voice]\nsession_listen_seconds = 4\n"
    );
    owner_only(&path);
    replacement
}

fn binary_retry_trace(fixture: &Fixture) {
    let rows = trace(&fixture.directory.path().join("trace"));
    for (event, count) in [
        ("bridge.ready", 3),
        ("settings.restart_prepared", 2),
        ("settings.restart_failed", 1),
        ("settings.restart_recovered", 1),
    ] {
        assert_eq!(rows.iter().filter(|row| row["event"] == event).count(), count);
    }
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("launches")).unwrap(),
        "started\nstarted\nstarted\n"
    );
}

#[cfg(unix)]
fn private_image(fixture: &Fixture, path: &Path) -> tokio::process::Command {
    let template = super::super::voice_entry::command(fixture);
    let template = template.as_std();
    std::fs::copy(template.get_program(), path).unwrap();
    let mut command = tokio::process::Command::new(path);
    command.args(template.get_args()).current_dir(fixture.directory.path());
    for (key, value) in template.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        } else {
            command.env_remove(key);
        }
    }
    command
}

pub(super) fn binary_home(fixture: &Fixture, port: u16) {
    super::super::voice_entry::configure(fixture, port);
    let base = fixture.directory.path().join("bridge.toml");
    let home = fixture.directory.path().join("agent-home");
    let text = format!(
        "{}\n[home]\ndir='{}'\n",
        std::fs::read_to_string(&base).unwrap(),
        home.display()
    );
    std::fs::write(base, text).unwrap();
}

async fn binary_pending_surface(fixture: &Fixture) {
    for _ in 0..2 {
        let snapshot = binary_snapshot(fixture).await;
        assert_eq!(snapshot["restart_pending"], true);
        assert_eq!(snapshot["settings"]["voice"]["session_listen_seconds"], 4);
        assert_eq!(
            snapshot["warnings"],
            json!(["Could not restart bridge; saved settings remain pending"])
        );
    }
    let saved = reqwest::Client::new()
        .post(settings_runtime_url(fixture, "/settings/v1/prompts/voice"))
        .json(&json!({"version":1,"text":"accepted after failed exec\n"}))
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), reqwest::StatusCode::OK);
    assert_eq!(
        saved.json::<Value>().await.unwrap(),
        json!({"version":1,"restart_pending":false})
    );
    let home = fixture.directory.path().join("agent-home");
    assert_eq!(
        std::fs::read_to_string(home.join("prompts/voice.md")).unwrap(),
        "accepted after failed exec\n"
    );
    let path = home.join("settings.toml");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[voice]\nsession_listen_seconds = 4\n"
    );
    owner_only(&path);
}

fn binary_failed_trace(fixture: &Fixture) {
    let rows = trace(&fixture.directory.path().join("trace"));
    for (event, count) in [
        ("bridge.ready", 2),
        ("settings.restart_prepared", 1),
        ("settings.restart_failed", 1),
        ("settings.restart_recovered", 1),
    ] {
        assert_eq!(rows.iter().filter(|row| row["event"] == event).count(), count);
    }
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("launches")).unwrap(),
        "started\nstarted\n"
    );
}
