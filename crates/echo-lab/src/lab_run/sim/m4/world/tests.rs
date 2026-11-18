//! T3 native capture, screen, exact provider/MCP output and owned cleanup.
use super::*;
use std::io::Write;

async fn journey(negative: bool) {
    let (mut case, owners) = lifecycle::start(false, negative).await;
    let (completion, timing) = case.speak().await.unwrap();
    assert_eq!(completion, json!({"event":"bridge.run_completed","run_id":"run-1"}));
    assert_eq!(timing.captured, world::fixture().len() + 25600);
    assert_eq!(timing.played, 9600);
    let sample = timing.sample();
    let instrumented = option_env!("CARGO_LLVM_COV").is_some();
    writeln!(
        std::io::stdout(),
        "M4_SAMPLE negative={negative} instrumented={instrumented} sample={sample:?}"
    )
    .unwrap();
    reply_latency(sample, instrumented).unwrap();
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

// Native full-binary branch coverage: max 3575.1736 ms across both outcomes,
// plus 25% scheduling headroom, rounded up to 100 ms = 4500 ms. See the
// 2026-11-18 M4 decision. The scenario and ordinary builds retain 1500 ms.
fn reply_latency(sample: Result<Duration, String>, instrumented: bool) -> Result<(), String> {
    let sample = sample?;
    let budget = Duration::from_millis(if instrumented { 4500 } else { 1500 });
    if sample > budget {
        return Err(format!(
            "M4 first reply PCM {sample:?} exceeds {budget:?} (instrumented={instrumented})"
        ));
    }
    Ok(())
}

#[test]
fn reply_latency_preserves_plain_budget_and_bounds_coverage_allowance() {
    for (instrumented, milliseconds) in [(false, 1500), (true, 4500)] {
        let budget = Duration::from_millis(milliseconds);
        assert_eq!(reply_latency(Ok(budget), instrumented), Ok(()));
        assert_eq!(
            reply_latency(Ok(budget + Duration::from_nanos(1)), instrumented),
            Err(format!(
                "M4 first reply PCM {:?} exceeds {budget:?} (instrumented={instrumented})",
                budget + Duration::from_nanos(1)
            ))
        );
        let missing = "wire: missing finite utterance endpoint".to_owned();
        assert_eq!(reply_latency(Err(missing.clone()), instrumented), Err(missing));
    }
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
