//! T3: configured HTTP saves coalesce while actual playback awaits Played.
//! Controlled child/media/device; generation cleanup is observed, not OS exec.
use super::super::tools::{Media, ready};
use super::*;
use echo_media::PeerState;
use std::sync::{Arc, OnceLock, atomic::Ordering};
use tokio::sync::watch;

#[tokio::test]
#[ignore = "T3 requires local Python and loopback MCP/settings"]
async fn t3_settings_configured_active_saves_wait_for_played_and_coalesce_cleanup() {
    let fixture = Fixture::new("chatgpt");
    std::fs::write(fixture.home.join("mode"), "tool-reply").unwrap();
    let spoof = echo_spoof_mcp::Spoof::start(echo_spoof_mcp::Options::default())
        .await
        .unwrap();
    let mut startup = settings_startup(&fixture);
    // The existing controlled child pins the MCP/session catalog. Home tools
    // have their own admission suite; this case exercises the settings surface.
    startup.base.home.as_mut().unwrap().agent_tools = echo_config::AgentTools {
        notes: false,
        prompts: false,
    };
    startup.base.mcp = super::super::tools::config(&fixture, spoof.url()).mcp;
    startup.config = startup.overlay.apply_to(&startup.base).unwrap();
    let path = fixture.directory.path().join("active-settings-trace");
    let owner = Configured::from_effective(startup, path.clone(), ([127, 0, 0, 1], 0).into()).unwrap();
    let observer = Observer::default();
    let closed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let work = owner.execute_with_settings(
        FakeConnector::new(observer.clone()),
        async || {
            Ok(Media {
                state: watch::channel(PeerState::New).0,
                closed: closed.clone(),
                setup: OnceLock::new(),
                lost: Arc::default(),
            })
        },
        vec![],
        std::future::pending(),
    );
    let completion = Box::pin(tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        tokio::pin!(work);
        tokio::select! {
            _ = &mut work => panic!("generation ended before Played"),
            () = save_during_playback(&fixture, &observer, &path, &closed) => (),
        }
        observer.finish(Ok(()));
        work.await.unwrap()
    }))
    .await
    .unwrap();
    assert!(completion.restart_requested);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    assert_eq!(observer.commands().last(), Some(&Command::Close));
    assert_coalesced(&fixture, &path, &mut completion.settings.unwrap());
    super::super::host::released(&fixture).await;
    spoof.shutdown().await.unwrap();
}

async fn save_during_playback(
    fixture: &Fixture,
    observer: &Observer,
    path: &Path,
    closed: &std::sync::atomic::AtomicUsize,
) {
    let (_control, _screen) = ready(fixture).await;
    observer.wake("held reply", vec![]);
    while !observer.commands().contains(&Command::Stop) {
        assert!(trace(path).iter().all(|row| row["event"] != "bridge.recovering"));
        tokio::task::yield_now().await;
    }
    for patch in [
        json!({"agent":{"daily_request_cap":"3"}}),
        json!({"codex":{"voice":"maple"}}),
    ] {
        assert_eq!(
            settings_runtime_patch(fixture, patch).await,
            json!({"version":1,"restart_pending":true})
        );
    }
    pending_snapshot(fixture).await;
    assert_eq!(closed.load(Ordering::SeqCst), 0);
    assert!(!observer.commands().contains(&Command::Close));
    let rows = trace(path);
    for event in ["settings.restart_prepared", "bridge.run_completed"] {
        assert_eq!(rows.iter().filter(|row| row["event"] == event).count(), 0);
    }
}

async fn pending_snapshot(fixture: &Fixture) {
    // A completed HTTP snapshot crosses the live host after both commits.
    let snapshot: Value = reqwest::Client::new()
        .get(settings_runtime_url(fixture, "/settings/v1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snapshot["settings"]["agent"]["daily_request_cap"], "3");
    assert_eq!(snapshot["settings"]["codex"]["voice"], "maple");
    assert_eq!(snapshot["restart_pending"], true);
}

fn assert_coalesced(fixture: &Fixture, path: &Path, host: &mut echo_bridge::bridge_settings::SettingsHost) {
    use echo_screen_server::{SettingsOperation, SettingsReply};
    let SettingsReply::Snapshot(snapshot) = host.transaction(
        SettingsOperation::Snapshot,
        echo_screen_contract::settings_v1::LinkStatusV1::Unconfigured,
    ) else {
        unreachable!("retained host refused snapshot")
    };
    assert!(snapshot.restart_pending);
    assert_eq!(snapshot.settings.agent.daily_request_cap.as_str(), "3");
    assert_eq!(snapshot.settings.codex.voice, "maple");
    let rows = trace(path);
    for event in ["bridge.run_completed", "settings.restart_prepared"] {
        assert_eq!(rows.iter().filter(|row| row["event"] == event).count(), 1);
    }
    let completed = rows
        .iter()
        .position(|row| row["event"] == "bridge.run_completed")
        .unwrap();
    let restarting = rows
        .iter()
        .position(|row| row["event"] == "settings.restart_prepared")
        .unwrap();
    assert!(completed < restarting);
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("launches")).unwrap(),
        "started\n"
    );
}

#[tokio::test]
#[ignore = "T3 requires local Python and loopback MCP/settings"]
async fn t3_settings_configured_saved_prompts_reach_next_call_without_restart() {
    let fixture = Fixture::new("chatgpt");
    std::fs::write(fixture.home.join("mode"), "tool-reply").unwrap();
    let spoof = echo_spoof_mcp::Spoof::start(echo_spoof_mcp::Options::default())
        .await
        .unwrap();
    let mut startup = settings_startup(&fixture);
    startup.base.home.as_mut().unwrap().agent_tools = echo_config::AgentTools {
        notes: false,
        prompts: false,
    };
    startup.base.mcp = super::super::tools::config(&fixture, spoof.url()).mcp;
    startup.config = startup.overlay.apply_to(&startup.base).unwrap();
    let path = fixture.directory.path().join("prompt-settings-trace");
    let owner = Configured::from_effective(startup, path.clone(), ([127, 0, 0, 1], 0).into()).unwrap();
    let observer = Observer::default();
    let closed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let work = owner.execute_with_settings(
        FakeConnector::new(observer.clone()),
        async || {
            Ok(Media {
                state: watch::channel(PeerState::New).0,
                closed: closed.clone(),
                setup: OnceLock::new(),
                lost: Arc::default(),
            })
        },
        vec![],
        async { stopping.await.unwrap() },
    );
    let inspect = async {
        saved_prompt_call(&fixture, &observer, &path).await;
        stop.send(()).unwrap();
    };
    let (result, ()) = Box::pin(tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        tokio::join!(work, inspect)
    }))
    .await
    .unwrap();
    assert!(!result.unwrap().restart_requested);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    saved_prompt_events(&path);
    assert_eq!(observer.commands().last(), Some(&Command::Close));
    assert_eq!(
        trace(&path)
            .iter()
            .filter(|row| row["event"] == "settings.restart_prepared")
            .count(),
        0
    );
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("launches")).unwrap(),
        "started\n"
    );
    super::super::host::released(&fixture).await;
    spoof.shutdown().await.unwrap();
}

async fn saved_prompt_call(fixture: &Fixture, observer: &Observer, path: &Path) {
    let (_control, _screen) = ready(fixture).await;
    save_prompts(fixture).await;
    observer.wake("saved prompt fixture", vec![]);
    while !observer.commands().contains(&Command::Stop) {
        assert!(trace(path).iter().all(|row| row["event"] != "bridge.recovering"));
        tokio::task::yield_now().await;
    }
    let read = |name| -> Value { serde_json::from_slice(&std::fs::read(fixture.home.join(name)).unwrap()).unwrap() };
    let runner = read("thread-config");
    let voice = read("realtime-config");
    // The saved text and the bridge's own session guidance (after the
    // runner's text, ahead of the voice's), then exactly one call-start
    // snapshot shared by the runner and the voice.
    let snapshot = |sent: &Value, prefix: String| -> String {
        let sent = sent.as_str().unwrap();
        let suffix = sent.strip_prefix(&prefix).unwrap();
        assert!(suffix.starts_with("\n[Call-start context: "));
        assert_eq!(sent.matches("[Call-start context:").count(), 1);
        suffix.to_owned()
    };
    let suffix = snapshot(
        &runner["baseInstructions"],
        format!("saved runner\n\n\n{}", echo_config::RUNNER_SESSION_GUIDANCE),
    );
    let voice = snapshot(
        &voice["prompt"],
        format!("{}\n\nsaved voice\n語 \n", echo_config::LIVE_SESSION_GUIDANCE),
    );
    assert_eq!(voice, suffix);
    observer.finish(Ok(()));
    while trace(path).iter().all(|row| row["event"] != "bridge.run_completed") {
        tokio::task::yield_now().await;
    }
}

async fn save_prompts(fixture: &Fixture) {
    for (kind, text) in [("voice", "saved voice\n語 \n"), ("runner", "saved runner\n")] {
        let response = reqwest::Client::new()
            .post(settings_runtime_url(fixture, &format!("/settings/v1/prompts/{kind}")))
            .json(&json!({"version":1,"text":text}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"version":1,"restart_pending":false})
        );
        assert_eq!(
            std::fs::read_to_string(fixture.directory.path().join(format!("agent-home/prompts/{kind}.md"))).unwrap(),
            text
        );
    }
}

fn saved_prompt_events(path: &Path) {
    let rows = trace(path);
    let saved: Vec<_> = rows
        .iter()
        .filter(|row| row["event"] == "prompt.saved")
        .map(|row| row["prompt"].as_str().unwrap())
        .collect();
    assert_eq!(saved, ["voice", "runner"]);
    assert_eq!(
        rows.iter()
            .filter(|row| row["event"] == "settings.reexec_attempt")
            .count(),
        0
    );
}
