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
        tokio::task::yield_now().await;
    }
    for patch in [
        json!({"agent":{"max_tool_calls":3}}),
        json!({"codex":{"voice":"marin"}}),
    ] {
        assert_eq!(
            settings_runtime_patch(fixture, patch).await,
            json!({"version":1,"restart_pending":true})
        );
    }
    // A completed HTTP snapshot crosses the live host after both commits.
    let snapshot: Value = reqwest::Client::new()
        .get(settings_runtime_url(fixture, "/settings/v1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snapshot["settings"]["agent"]["max_tool_calls"], 3);
    assert_eq!(snapshot["settings"]["codex"]["voice"], "marin");
    assert_eq!(snapshot["restart_pending"], true);
    assert_eq!(closed.load(Ordering::SeqCst), 0);
    assert!(!observer.commands().contains(&Command::Close));
    let rows = trace(path);
    assert_eq!(
        rows.iter()
            .filter(|row| row["event"] == "settings.restart_prepared")
            .count(),
        0
    );
    assert_eq!(
        rows.iter().filter(|row| row["event"] == "bridge.run_completed").count(),
        0
    );
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
    assert_eq!(snapshot.settings.agent.max_tool_calls, 3);
    assert_eq!(snapshot.settings.codex.voice, "marin");
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
