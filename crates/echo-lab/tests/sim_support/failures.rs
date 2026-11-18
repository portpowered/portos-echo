use super::{Session, records};
use serde_json::json;

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn m1_restart_diagnostics_preserve_child_exit_reason_and_cleanup() {
    for missing in [false, true] {
        let session = Session::new();
        session.scenario(json!([{"say":"fixture.pcm"}]), json!({}));
        let output = session.script(&json!({"exit_code":7,"missing_trace":missing,
            "records":[{"event":"bridge.error","elapsed_ms":1,"reason":"wyoming",
                "payload":"synthetic-secret-sentinel"}]}));
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("step 0:"), "{stderr}");
        assert!(
            stderr.contains("exit status: 7") || stderr.contains("exit code: 7"),
            "{stderr}"
        );
        if missing {
            assert!(stderr.contains("\"trace\":\"unavailable\""), "{stderr}");
            assert!(!stderr.contains("\"event\":\"bridge.error\""), "{stderr}");
        } else {
            assert!(stderr.contains("\"event\":\"bridge.error\""), "{stderr}");
            assert!(stderr.contains("\"reason\":\"wyoming\""), "{stderr}");
        }
        assert!(!stderr.contains("synthetic-secret-sentinel"));
        assert_eq!(session.report()["result"], "fail");
        session.endpoints();
    }
}

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
        json!({"press":"unknown"}),
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

fn observation_failures() -> [(serde_json::Value, serde_json::Value, serde_json::Value, &'static str); 6] {
    [
        (
            json!({"expect_screen":{"state":"speaking"}}),
            json!({"ui":{"state":"listening"}}),
            json!({}),
            "step 0: screen expectation: latest UiState did not match",
        ),
        (
            json!({"expect_screen":{"state":"speaking"}}),
            json!({}),
            json!({}),
            "step 0: screen expectation: missing observation file",
        ),
        (
            json!({"expect_speech_contains":["missing"]}),
            json!({}),
            json!({}),
            "step 0: speech expectation: recorded reply did not match",
        ),
        (
            json!({"touch":{"target":"missing"}}),
            json!({}),
            json!({}),
            "step 0: sim: unsupported step capability",
        ),
        (
            json!({"expect_frame":"frame.jpg"}),
            json!({}),
            json!({}),
            "step 0: sim: unsupported step capability",
        ),
        (
            json!({"expect_trace":["wyoming.run_ended"]}),
            json!({}),
            json!({"unknown":5}),
            "step 1: budget: unsupported metric",
        ),
    ]
}

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn missing_wrong_and_unsupported_observations_fail_without_following_steps() {
    // Each pair owns independent processes, endpoints and files.
    for cases in observation_failures().chunks(2) {
        std::thread::scope(|scope| {
            for (step, script, budgets, error) in cases.iter().cloned() {
                scope.spawn(move || {
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
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    assert!(stderr.starts_with(&format!("echo-lab: scenario.yaml: {error}; bridge diagnostics:")));
                    let report = session.report();
                    assert_eq!(report["result"], "fail");
                    assert_eq!(report["checks"][if has_budget { "step_1" } else { "step_0" }], false);
                    assert_eq!(report["checks"]["world_closed"], true);
                    session.endpoints();
                });
            }
        });
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
    parallel_worlds(&first, &second);
    first.endpoints();
    second.endpoints();
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

fn parallel_worlds(first: &Session, second: &Session) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let script = json!({"records":records(),"ready_barrier":listener.local_addr().unwrap().to_string()});
        for session in [first, second] {
            std::fs::write(session.0.join("script.json"), script.to_string()).unwrap();
        }
        let mut one = first
            .command(env!("CARGO_BIN_EXE_echo-lab-sim-script"))
            .spawn()
            .unwrap();
        let mut two = second
            .command(env!("CARGO_BIN_EXE_echo-lab-sim-script"))
            .spawn()
            .unwrap();
        let held = async {
            let mut owners = Vec::new();
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let endpoints: Vec<std::net::SocketAddr> = serde_json::from_str(&line).unwrap();
                assert_eq!(endpoints.len(), 3);
                owners.push((reader, endpoints));
            }
            let distinct = owners[0].1.iter().all(|address| !owners[1].1.contains(address));
            // Both bridges are still blocked before ready, with their real
            // host/spoof/Live owners alive. Release even when comparison fails.
            for (stream, _) in &mut owners {
                stream.get_mut().write_all(b"G").await.unwrap();
            }
            distinct
        };
        let distinct = tokio::time::timeout(std::time::Duration::from_secs(20), held).await;
        // Timeout drops the control connections. Let each runner collect its
        // children through its existing readiness/cleanup deadlines.
        let statuses = [one.wait().unwrap().code(), two.wait().unwrap().code()];
        assert!(distinct.unwrap(), "simultaneous worlds shared an owned endpoint");
        assert_eq!(statuses, [Some(0), Some(0)]);
    });
}
