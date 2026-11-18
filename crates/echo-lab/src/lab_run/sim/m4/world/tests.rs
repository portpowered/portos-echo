//! T3 native capture, screen, exact provider/MCP output and owned cleanup.
use super::*;

async fn journey(negative: bool) {
    let (mut case, owners) = lifecycle::start(false, negative).await;
    let (completion, timing) = case.speak().await.unwrap();
    assert_eq!(completion, json!({"event":"bridge.run_completed","run_id":"run-1"}));
    assert_eq!(timing.captured, world::fixture().len() + 25600);
    assert_eq!(timing.played, 9600);
    assert!(timing.sample().unwrap() <= Duration::from_millis(1500));
    assert_eq!(case.close().await, Ok(()));
    assert_eq!(
        case.transcript.as_deref(),
        Some(if negative {
            super::super::model::FALLBACK
        } else {
            super::super::model::SUCCESS
        })
    );
    assert_trace(&case.trace);
    owners.closed(&case);
    assert_eq!(case.close().await, Ok(()));
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host, prebuilt bridge and offline Codex pin"]
async fn t3_m4_pinned_host_positive_finite_capture_screen_reply_and_cleanup() {
    journey(false).await;
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host, prebuilt bridge and offline Codex pin"]
async fn t3_m4_pinned_host_http500_finite_capture_screen_fallback_and_cleanup() {
    journey(true).await;
}

#[test]
fn destination_collision_fails_before_startup_and_preserves_previous_owner() {
    let owner = Root::new().unwrap();
    std::fs::write(owner.0.join("retained"), "previous owner").unwrap();
    assert_eq!(
        Root::create(owner.0.clone()).err(),
        Some("m2: cannot create private root".into())
    );
    assert_eq!(
        std::fs::read_to_string(owner.0.join("retained")).unwrap(),
        "previous owner"
    );
}

fn assert_trace(trace: &Trace) {
    let selectors = crate::lab_run::sim::selectors::parse(&[
        json!("delegation.created"),
        json!("item/tool/call {name: test.echo, count: 1}"),
        json!("tools/call {name: test.echo, count: 1}"),
        json!("delegation.context.append"),
    ])
    .unwrap();
    assert_eq!(
        crate::lab_run::sim::selectors::check(&trace.records, &selectors, Some("run-1")),
        Ok(())
    );
    for forbidden in ["synthetic-m4-key", "test-pair", "delegated echo"] {
        assert!(!serde_json::to_string(&trace.records).unwrap().contains(forbidden));
    }
}
