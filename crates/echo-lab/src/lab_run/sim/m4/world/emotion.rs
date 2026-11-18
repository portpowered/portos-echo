//! Interrupted and failed M6 presentations on canonical Codex ownership.
use super::lifecycle::start;
use super::*;

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host, prebuilt bridge and offline Codex pin"]
async fn m6_t3_http500_never_animates_and_plays_honest_fallback_once() {
    let (mut case, owners) = start(true, true).await;
    let (completion, timing) = case.speak().await.unwrap();
    assert_eq!(completion, json!({"event":"bridge.run_completed","run_id":"run-1"}));
    assert_eq!(timing.played, 9600);
    assert_eq!(case.close().await, Ok(()));
    assert_eq!(case.transcript.as_deref(), Some(super::super::model::FALLBACK));
    let expected = super::super::emotion::sequence();
    let states: Vec<_> = case.ui_states.iter().map(|f| f["state"].clone()).collect();
    assert_eq!(
        states,
        [
            expected[0].clone(),
            expected[1].clone(),
            expected[2].clone(),
            expected[5].clone(),
            expected[6].clone()
        ]
    );
    assert!(
        case.ui_states
            .windows(2)
            .all(|p| p[0]["seq"].as_u64() < p[1]["seq"].as_u64())
    );
    let playback = case.playback.as_ref().unwrap();
    assert_eq!(playback["event"], "host.ack");
    assert_eq!(playback["pcm_bytes"], 9600);
    assert!(playback["nonzero_bytes"].as_u64().unwrap() > 0);
    assert!(playback["nonzero_samples"].as_u64().unwrap() >= 2400);
    assert!(playback["audible_samples"].as_u64().unwrap() >= 2400);
    owners.closed().await;
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host, prebuilt bridge and offline Codex pin"]
async fn m6_t3_eof_during_animation_reaps_pending_expiry_model_and_all_listeners() {
    let (mut case, owners) = start(true, false).await;
    let mut screen = Screen::connect(&owners.root).await.unwrap();
    screen.expect("ready").await.unwrap();
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let observation = async {
        for state in ["listening", "connected", "emotion"] {
            screen.expect(state).await.unwrap();
        }
        cancel.send(()).unwrap();
    };
    let interrupted = async {
        tokio::select! {
            result = case.speak() => panic!("reply completed before animation cancellation: {result:?}"),
            result = cancelled => result.unwrap(),
        }
    };
    tokio::join!(observation, interrupted);
    // Stop advancing the bridge clock at the animation, then finish the native
    // tool-result round trip before EOF. Its progress is independent of the
    // paused expiry timer and must not select a different partial-run oracle.
    let observed = &mut case.provider.as_mut().unwrap().observed;
    observed
        .wait_for(|snapshot| snapshot["appends"] == json!(["delegation-local"]))
        .await
        .unwrap();
    let timing = case.relay.as_ref().unwrap().timing();
    assert_eq!(timing.played, 0);
    assert!(case.speaking.is_none());
    let result = case.close().await;
    assert_eq!(
        result,
        Err("m4: exact pinned provider/model observation mismatch".into())
    );
    assert_eq!(case.snapshot["delegations"], json!(["delegation-local"]));
    assert_eq!(case.snapshot["appends"], json!(["delegation-local"]));
    assert_eq!(
        case.snapshot["models"],
        json!([
            {"tools":["tool_0","tool_1","tool_2","tool_3"],"outputs":0},
            {"tools":["tool_0","tool_1","tool_2","tool_3"],"outputs":1}
        ])
    );
    assert_eq!(case.snapshot["transcripts"], json!([]));
    assert_eq!(screen.next().await, Err("screen: EOF before observation".into()));
    assert!(!case.trace.records.iter().any(|r| r["event"] == "bridge.run_completed"));
    owners.closed().await;
    assert_eq!(case.close().await, Ok(()));
}
