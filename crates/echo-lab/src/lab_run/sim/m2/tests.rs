use super::*;

fn measured(case: &str) -> Value {
    let frames: Vec<_> = oracle::expected(case)["sequence"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, state)| json!({"seq":i+1,"state":state}))
        .collect();
    json!({"case":case,"calls":u64::from(case == "success"),"provider_wire":true,
        "refreshes":u64::from(case == "expired"),"calls_after_detection":0,"frames":frames,
        "trace":if case == "success" { json!([{"event":"live.closed","reason":"idle"}]) }
            else { json!([{"event":"auth.relink_required","run_id":"run-1"},{"event":"run.refused","reason":"relink_required","run_id":"run-2"}]) }})
}

#[test]
fn m2_exact_oracle_rejects_missing_duplicate_stale_and_late_evidence() {
    for case in ["success", "invalid", "expired"] {
        let actual = measured(case);
        assert_eq!(oracle::check(&actual), Ok(()));
        for field in ["calls", "refreshes", "provider_wire", "frames", "trace"] {
            let mut wrong = actual.clone();
            wrong.as_object_mut().unwrap().remove(field);
            assert!(oracle::check(&wrong).is_err(), "missing {field}");
        }
        for field in ["calls", "refreshes", "calls_after_detection"] {
            let mut wrong = actual.clone();
            wrong[field] = 2.into();
            if case != "success" || field != "calls_after_detection" {
                assert!(oracle::check(&wrong).is_err());
            }
        }
        let mut wrong = actual.clone();
        wrong["frames"][1]["seq"] = 1.into();
        assert!(oracle::check(&wrong).is_err());
        wrong = actual.clone();
        wrong["frames"][1]["state"] = json!({"state":"connected"});
        assert!(oracle::check(&wrong).is_err());
        wrong = actual.clone();
        let duplicate = wrong["trace"][0].clone();
        wrong["trace"].as_array_mut().unwrap().push(duplicate);
        assert!(oracle::check(&wrong).is_err());
    }
    let mut wrong = measured("success");
    wrong["trace"] = json!([]);
    assert_eq!(
        oracle::check(&wrong),
        Err("m2: success close count expected 1, observed 0".into())
    );
}

#[test]
fn m2_unknown_injections_are_rejected_before_effects() {
    for injection in ["unknown", "expired", "invalid"] {
        assert!(negative(&json!({"id":"m2-live-connect","negative":[{"inject":{"credential":injection}}]})).is_err());
    }
}

fn options() -> crate::lab_run::run_options::Options {
    let binary = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug")
        .join(if cfg!(windows) {
            "echo-bridge.exe"
        } else {
            "echo-bridge"
        });
    crate::lab_run::run_options::parse(&[
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/scenarios/m2-live-connect.yaml")
            .to_string_lossy()
            .into_owned(),
        "--driver".into(),
        "sim".into(),
        "--bridge".into(),
        binary.to_string_lossy().into_owned(),
    ])
    .unwrap()
}

fn utterance() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/lab/w1-utterance.pcm")
}

async fn closed(measured: &Value) {
    for address in measured["endpoints"].as_array().unwrap() {
        assert!(tokio::net::TcpStream::connect(address.as_str().unwrap()).await.is_err());
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production bridge and pinned hassmic-host"]
async fn m2_t3_concurrent_cases_are_independent_and_reap_all_endpoints() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let options = options();
    let utterance = utterance();
    let ((success, a), (expired, b)) = tokio::join!(
        journey::run(&options, first.path(), "success", &utterance),
        journey::run(&options, second.path(), "expired", &utterance)
    );
    assert_eq!(a, Ok(()));
    assert_eq!(b, Ok(()));
    assert_eq!(success["calls"], 1);
    assert_eq!(expired["calls"], 0);
    assert_eq!(expired["refreshes"], 1);
    assert_eq!(std::fs::read_dir(first.path()).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(second.path()).unwrap().count(), 0);
    closed(&success).await;
    closed(&expired).await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production bridge and pinned hassmic-host"]
async fn m2_t3_startup_failure_and_input_error_retain_partial_evidence_and_reap() {
    for failure in ["startup", "input"] {
        let root = tempfile::tempdir().unwrap();
        let mut options = options();
        if failure == "startup" {
            options.bridge = Some(root.path().join("missing-bridge"));
        }
        let (measured, result) = journey::run(
            &options,
            root.path(),
            if failure == "startup" { "success" } else { "invalid" },
            &root.path().join("missing-pcm"),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        if failure == "input" {
            assert_eq!(measured["frames"].as_array().unwrap().len(), 1);
        }
        closed(&measured).await;
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production bridge and pinned hassmic-host"]
async fn m2_t3_cancellation_after_wake_reaps_and_retains_observed_frames() {
    let root = tempfile::tempdir().unwrap();
    let cancellation = tempfile::tempdir().unwrap();
    let mut options = options();
    let cancel = cancellation.path().join("cancel");
    options.cancel = Some(cancel.clone());
    let trace = root.path().join("private-success/bridge.private.jsonl");
    let utterance = utterance();
    let request_cancel = async {
        tokio::time::timeout(DEADLINE, async {
            loop {
                if tokio::fs::read_to_string(&trace)
                    .await
                    .is_ok_and(|text| text.contains("run.started"))
                {
                    std::fs::write(cancel, "cancel").unwrap();
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    };
    let ((measured, result), ()) = tokio::join!(
        journey::run(&options, root.path(), "success", &utterance),
        request_cancel
    );
    assert!(result.unwrap_err().contains("cancel"));
    assert_eq!(
        measured["frames"][0]["state"],
        json!({"state":"ready","message":"Echo Live ready"})
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    closed(&measured).await;
}
