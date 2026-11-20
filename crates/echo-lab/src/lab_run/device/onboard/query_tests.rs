//! Device adapter component tests with a fake Shell/trace; no accounts, hardware or network.
use super::*;
use crate::lab_run::device::onboard::tests::{World, remote, world};
use serde_json::{Value, json};
use std::{cell::RefCell, fmt::Write, rc::Rc};

const CONFIG: &str = include_str!("../../../../../../examples/bridge.echo.toml");

fn runtime() -> (Rc<RefCell<World>>, Runtime) {
    let world = world();
    let root = tempfile::tempdir().unwrap();
    let local = root.path().join("config");
    std::fs::write(&local, CONFIG).unwrap();
    let mut owned = remote(&world);
    owned.stage(&local).unwrap();
    owned.attempted = true;
    world.borrow_mut().alive = true;
    let mut state = Runtime::default();
    state.remote = Some(owned);
    state.configure_portos(CONFIG).unwrap();
    state.started = true;
    state.generation = 1;
    (world, state)
}

fn append(world: &Rc<RefCell<World>>, record: &Value) {
    writeln!(world.borrow_mut().trace, "{record}").unwrap();
}

fn returned(world: &Rc<RefCell<World>>, name: &str, run: &str, outcome: Option<&str>, ordinal: u64) {
    append(
        world,
        &json!({"event":"tools/call","elapsed_ms":10,"run_id":run,
        "thread_id":"thread","turn_id":"turn","call_id":format!("call-{ordinal}"),"name":name}),
    );
    if let Some(outcome) = outcome {
        append(
            world,
            &json!({"event":"mcp.tool.completed","elapsed_ms":11,
            "provider_auth_mode":"chatgpt","server":"portos","name":name,
            "attempt_count":ordinal,"outcome":outcome}),
        );
    }
}

async fn completed(world: &Rc<RefCell<World>>, state: &mut Runtime, run: &str) {
    for (event, elapsed) in [
        ("wyoming.run_started", 12),
        ("wyoming.mic_frames", 13),
        ("wyoming.run_ended", 14),
        ("bridge.run_completed", 15),
    ] {
        append(
            world,
            &json!({"event":event,"elapsed_ms":elapsed,"run_id":run,"count":12}),
        );
    }
    state.complete(7).await.unwrap();
}

#[tokio::test]
async fn t3_on_device_mcp_query_requires_fresh_completed_endpoint_query_result() {
    let (world, mut state) = runtime();
    state.arm_wake().unwrap();
    let query = state.begin_mcp_query(McpAction::Call).unwrap();
    returned(&world, "endpoint_query", "query-run", Some("success"), 1);
    assert_eq!(state.finish_mcp_query(&query), Err(QUERY.into()));
    assert!(state.invalid);

    let (world, mut state) = runtime();
    state.arm_wake().unwrap();
    let query = state.begin_mcp_query(McpAction::Call).unwrap();
    returned(&world, "endpoint_query", "query-run", Some("success"), 1);
    completed(&world, &mut state, "query-run").await;
    state.finish_mcp_query(&query).unwrap();
    assert_eq!(state.portos.as_ref().unwrap().measure().unwrap().tool_call, 1);
    assert_eq!(
        state.wakes,
        [json!({"bridge_generation":1,"run_id":"query-run","mic_frames":12})]
    );
}

#[tokio::test]
async fn t3_on_device_mcp_query_error_arrival_and_unrelated_success_cannot_pass() {
    for (name, run, outcome) in [
        ("endpoint_query", "query-run", Some("error")),
        ("endpoint_query", "query-run", None),
        ("endpoint_show", "query-run", Some("success")),
        ("endpoint_query", "other-run", Some("success")),
    ] {
        let (world, mut state) = runtime();
        state.arm_wake().unwrap();
        let query = state.begin_mcp_query(McpAction::Call).unwrap();
        returned(&world, name, run, outcome, 1);
        completed(&world, &mut state, "query-run").await;
        assert!(state.finish_mcp_query(&query).is_err());
        assert!(state.invalid);
        let root = tempfile::tempdir().unwrap();
        let mut artifacts = Artifacts::create(root.path().join("report")).unwrap();
        assert!(state.close(root.path(), &mut artifacts).await.is_err());
        assert!(artifacts.link_counts.is_none());
        assert!(!world.borrow().alive);
    }
}

#[tokio::test]
async fn t3_on_device_mcp_query_rejects_old_run_receipts_and_generation_changes() {
    for changed_generation in [false, true] {
        let (world, mut state) = runtime();
        returned(&world, "endpoint_query", "old-run", Some("success"), 1);
        state.arm_wake().unwrap();
        state.run = Some("old-run".into());
        let query = state.begin_mcp_query(McpAction::Call).unwrap();
        completed(&world, &mut state, "query-run").await;
        if changed_generation {
            state.generation += 1;
        }
        assert_eq!(state.finish_mcp_query(&query), Err(QUERY.into()));
        assert!(state.invalid);
    }
}

#[tokio::test]
async fn t3_on_device_mcp_probe_cannot_add_success_or_reuse_an_earlier_completed_run() {
    for outcome in [None, Some("error"), Some("success")] {
        let (world, mut state) = runtime();
        state.arm_wake().unwrap();
        let query = state.begin_mcp_query(McpAction::Probe).unwrap();
        if outcome.is_some() {
            returned(&world, "endpoint_query", "probe-run", outcome, 1);
        }
        completed(&world, &mut state, "probe-run").await;
        assert_eq!(state.finish_mcp_query(&query).is_ok(), outcome != Some("success"));
    }
    let (world, mut state) = runtime();
    state.arm_wake().unwrap();
    completed(&world, &mut state, "probe-run").await;
    let query = state.begin_mcp_query(McpAction::Probe).unwrap();
    assert_eq!(state.finish_mcp_query(&query), Err(QUERY.into()));
}

#[tokio::test]
async fn t3_on_device_mcp_query_cancelled_before_completion_has_no_passing_counts_and_cleans_up() {
    let (world, mut state) = runtime();
    let query = state.begin_mcp_query(McpAction::Call).unwrap();
    returned(&world, "endpoint_query", "query-run", None, 1);
    let root = tempfile::tempdir().unwrap();
    let cancel = root.path().join("cancel");
    std::fs::write(&cancel, "cancel").unwrap();
    assert_eq!(
        crate::lab_run::device::runtime::controlled(state.complete(7), Some(&cancel)).await,
        Err("run cancelled".into())
    );
    assert!(state.finish_mcp_query(&query).is_err());
    let mut artifacts = Artifacts::create(root.path().join("report")).unwrap();
    assert!(state.close(root.path(), &mut artifacts).await.is_err());
    assert!(artifacts.link_counts.is_none());
    assert!(!world.borrow().alive);
    assert!(world.borrow().ran("rm -f"));
}

#[test]
fn mcp_device_query_refuses_inactive_host_or_wrong_action_before_playback() {
    for change in 0..3 {
        let (_world, mut state) = runtime();
        match change {
            0 => state.started = false,
            1 => state.remote = None,
            _ => (),
        }
        let action = if change == 2 {
            McpAction::Refresh
        } else {
            McpAction::Call
        };
        assert!(state.begin_mcp_query(action).is_err());
        assert_eq!(state.wakes, Vec::<Value>::new());
    }
}

#[test]
fn mcp_device_query_dispatch_requires_nominated_speech_and_revokes_prior_counts() {
    use crate::lab_run::device::{Configured, onboard::tests::parsed};
    for action in [McpAction::Call, McpAction::Probe] {
        let root = tempfile::tempdir().unwrap();
        let options = parsed(root.path(), "m1-hassmic-bridge");
        let mut errors = Vec::new();
        let mut driver = Configured::new(&options, CONFIG.into(), &mut errors).ok().unwrap();
        let (world, state) = runtime();
        driver.state = state;
        let mut artifacts = Artifacts::create(root.path().join("report")).unwrap();
        artifacts.link_counts = Some(echo_lab::lab_mcp::LinkCounts::default());
        let calls_before_query = world.borrow().calls.clone();
        assert_eq!(
            driver.mcp(action, 7, &mut artifacts),
            Err("device: M5 query requires --wake-clip naming only Echo test light".into())
        );
        assert!(driver.state.invalid);
        assert!(artifacts.link_counts.is_none());
        assert_eq!(world.borrow().calls, calls_before_query);
        assert_eq!(driver.state.plays, Vec::<Value>::new());
    }
}
