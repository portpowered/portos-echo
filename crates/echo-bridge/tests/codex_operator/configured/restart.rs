//! T3: recovery remounts production HTTP/child/device owners, without OS exec.
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
