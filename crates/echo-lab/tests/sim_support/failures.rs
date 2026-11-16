use super::{Session, records};
use serde_json::json;

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn unlaunchable_bridge_closes_the_world_and_a_fresh_run_ignores_old_artifacts() {
    let failed = Session::new();
    failed.scenario(json!([{"press":"action"}]), json!({}));
    let invalid = failed.0.join("invalid.exe");
    std::fs::write(&invalid, b"not an executable").unwrap();
    let output = failed.command(invalid.to_str().unwrap()).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("launch: cannot spawn owned process"));
    assert_eq!(failed.report()["checks"], json!({"step_0":false,"world_closed":true}));
    failed.endpoints();
    let previous = std::fs::read(failed.0.join("artifacts/report.json")).unwrap();
    let fresh = Session::new();
    fresh.scenario(json!([{"expect_trace":["wyoming.run_ended"]}]), json!({}));
    assert_eq!(fresh.script(&json!({"records":records()})).status.code(), Some(0));
    assert_eq!(fresh.report()["result"], "pass");
    fresh.endpoints();
    assert_eq!(std::fs::read(failed.0.join("artifacts/report.json")).unwrap(), previous);
}

#[test]
fn sim_preflight_refuses_unsupported_controls_setups_and_missing_executables() {
    for step in [
        json!({"press":"mute"}),
        json!({"press":{"key":"action","extra":true}}),
        json!({"expect_trace":[]}),
        json!({"say":"missing.pcm"}),
        json!({"say":"fixture.wav"}),
    ] {
        let session = Session::new();
        session.scenario(json!([step]), json!({}));
        let output = session
            .command(env!("CARGO_BIN_EXE_echo-lab-sim-bridge"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(!session.0.join("artifacts").exists());
    }
    for change in [
        json!({"setup":{"mcp":"portos-pilot"}}),
        json!({"setup":{"backend":"muse"}}),
        json!({"negative":[{"inject":{"tool":"bad"},"expect_trace":["tool.failed"]}]}),
    ] {
        let session = Session::new();
        session.scenario(json!([{"press":"action"}]), json!({}));
        let path = session.0.join("scenario.yaml");
        let mut scenario: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        scenario
            .as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        std::fs::write(path, scenario.to_string()).unwrap();
        assert_eq!(
            session
                .command(env!("CARGO_BIN_EXE_echo-lab-sim-bridge"))
                .output()
                .unwrap()
                .status
                .code(),
            Some(1)
        );
        assert!(!session.0.join("artifacts").exists());
    }
    let session = Session::new();
    session.scenario(json!([{"press":"action"}]), json!({}));
    assert_eq!(
        session.command("./missing-bridge").output().unwrap().status.code(),
        Some(1)
    );
    assert!(!session.0.join("artifacts").exists());
}

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn missing_wrong_and_unsupported_observations_fail_without_following_steps() {
    for (step, script, budgets) in [
        (
            json!({"expect_screen":{"state":"speaking"}}),
            json!({"ui":{"state":"listening"}}),
            json!({}),
        ),
        (json!({"expect_screen":{"state":"speaking"}}), json!({}), json!({})),
        (json!({"expect_speech_contains":["missing"]}), json!({}), json!({})),
        (json!({"touch":{"target":"missing"}}), json!({}), json!({})),
        (json!({"expect_frame":"frame.jpg"}), json!({}), json!({})),
        (
            json!({"expect_trace":["wyoming.run_ended"]}),
            json!({}),
            json!({"unknown":5}),
        ),
    ] {
        let session = Session::new();
        let has_budget = budgets.as_object().unwrap().len() == 1;
        session.scenario(
            if has_budget {
                json!([step])
            } else {
                json!([step,{"press":"action"}])
            },
            budgets,
        );
        let mut fixture = json!({"records":records()});
        fixture
            .as_object_mut()
            .unwrap()
            .extend(script.as_object().unwrap().clone());
        let output = session.script(&fixture);
        assert_eq!(output.status.code(), Some(1));
        let report = session.report();
        assert_eq!(report["result"], "fail");
        assert_eq!(report["checks"][if has_budget { "step_1" } else { "step_0" }], false);
        assert_eq!(report["checks"]["world_closed"], true);
        session.endpoints();
    }
}

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn missing_trace_readiness_and_unwritable_publication_never_claim_success() {
    for script in [
        json!({"missing_trace":true}),
        json!({"no_ready":true,"cancel":true}),
        json!({"block_report":true}),
    ] {
        let session = Session::new();
        session.scenario(json!([{"expect_screen":{"state":"speaking"}}]), json!({}));
        let mut fixture = json!({"records":records(),"ui":{"state":"speaking"}});
        fixture
            .as_object_mut()
            .unwrap()
            .extend(script.as_object().unwrap().clone());
        std::fs::write(session.0.join("script.json"), fixture.to_string()).unwrap();
        let output = session
            .command(env!("CARGO_BIN_EXE_echo-lab-sim-script"))
            .args(["--cancel-file", "cancel"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        if script["block_report"] == true {
            assert!(!session.0.join("artifacts/report.json").exists());
        } else {
            assert_eq!(session.report()["result"], "fail");
        }
        session.endpoints();
    }
}

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn independent_worlds_have_distinct_endpoints_and_shared_destination_has_one_owner() {
    let first = Session::new();
    let second = Session::new();
    for session in [&first, &second] {
        session.scenario(json!([{"expect_trace":["wyoming.run_ended"]}]), json!({}));
    }
    std::thread::scope(|scope| {
        let a = scope.spawn(|| first.script(&json!({"records":records()})));
        let b = scope.spawn(|| second.script(&json!({"records":records()})));
        assert_eq!(a.join().unwrap().status.code(), Some(0));
        assert_eq!(b.join().unwrap().status.code(), Some(0));
    });
    let a = first.endpoints();
    let b = second.endpoints();
    assert!(a.iter().all(|address| !b.contains(address)));
    assert_eq!(first.report()["checks"], second.report()["checks"]);
    let report = std::fs::read(first.0.join("artifacts/report.json")).unwrap();
    assert_eq!(
        first
            .command(env!("CARGO_BIN_EXE_echo-lab-sim-script"))
            .output()
            .unwrap()
            .status
            .code(),
        Some(1)
    );
    assert_eq!(std::fs::read(first.0.join("artifacts/report.json")).unwrap(), report);
    let race = Session::new();
    race.scenario(json!([{"expect_trace":["wyoming.run_ended"]}]), json!({}));
    std::fs::write(race.0.join("script.json"), json!({"records":records()}).to_string()).unwrap();
    let mut one = race.command(env!("CARGO_BIN_EXE_echo-lab-sim-script")).spawn().unwrap();
    let mut two = race.command(env!("CARGO_BIN_EXE_echo-lab-sim-script")).spawn().unwrap();
    let mut codes = [one.wait().unwrap().code(), two.wait().unwrap().code()];
    codes.sort();
    assert_eq!(codes, [Some(0), Some(1)]);
    assert_eq!(race.report()["result"], "pass");
    race.endpoints();
}
