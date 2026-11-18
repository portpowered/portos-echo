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
        "refreshes":if case == "expired" { oracle::EXPIRED_REFRESHES } else { 0 },"wyoming_admissions":0,"sidebands":0,"external":0,"exit_code":1,"stdout":"","stderr":"echo-bridge: authorization\n","auth_unchanged":true,"home_released":true,"frames":frames,
        "trace":if case == "success" { json!([{"event":"live.closed","reason":"idle"}]) }
            else { json!([{"event":"auth.relink_required"}]) }})
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
        for field in ["calls", "refreshes", "wyoming_admissions"] {
            let mut wrong = actual.clone();
            wrong[field] = 3.into();
            if case != "success" || field != "wyoming_admissions" {
                assert!(oracle::check(&wrong).is_err());
            }
        }
        let mut wrong = actual.clone();
        wrong["frames"][0]["seq"] = 0.into();
        assert!(oracle::check(&wrong).is_err());
        wrong = actual.clone();
        wrong["frames"][0]["state"] = json!({"state":"connected"});
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
fn m2_auth_oracle_rejects_unmeasured_cleanup_and_pre_admission_effects() {
    for case in ["invalid", "expired"] {
        let valid = measured(case);
        for field in [
            "wyoming_admissions",
            "sidebands",
            "external",
            "exit_code",
            "stdout",
            "stderr",
            "auth_unchanged",
            "home_released",
        ] {
            let mut wrong = valid.clone();
            wrong.as_object_mut().unwrap().remove(field);
            assert!(oracle::check(&wrong).is_err(), "missing {field}");
        }
        for event in [
            "bridge.ready",
            "run.started",
            "live.connected",
            "mic.completed",
            "reply.played",
            "run.refused",
        ] {
            let mut wrong = valid.clone();
            wrong["trace"].as_array_mut().unwrap().push(json!({"event":event}));
            assert!(oracle::check(&wrong).is_err(), "unexpected {event}");
        }
        let mut wrong = valid.clone();
        wrong["refreshes"] = 1.into();
        assert!(oracle::check(&wrong).is_err());
        for field in ["auth_unchanged", "home_released"] {
            let mut wrong = valid.clone();
            wrong[field] = false.into();
            assert!(oracle::check(&wrong).is_err());
        }
    }
}

#[test]
fn m2_unknown_injections_are_rejected_before_effects() {
    for injection in ["unknown", "expired", "invalid"] {
        assert!(negative(&json!({"id":"m2-live-connect","negative":[{"inject":{"credential":injection}}]})).is_err());
    }
}

#[test]
fn m2_codex_oracle_requires_exact_frames_and_measured_audibility() {
    let mut valid = measured("success");
    valid["runtime"] = json!("codex");
    valid["reply_pcm"] = json!({"pcm_bytes":9600,"nonzero_samples":4800,"audible_samples":2400});
    valid["trace"].as_array_mut().unwrap().extend([
        json!({"event":"mic.completed","count":10,"dropped_chunks":0,"tail_samples":0}),
        json!({"event":"reply.played","outcome":"played","frames":10,"received_frames":10,"dropped_frames":0}),
    ]);
    assert_eq!(oracle::check(&valid), Ok(()));
    for field in ["pcm_bytes", "nonzero_samples", "audible_samples"] {
        let mut wrong = valid.clone();
        wrong["reply_pcm"].as_object_mut().unwrap().remove(field);
        assert!(oracle::check(&wrong).is_err(), "missing {field}");
        wrong = valid.clone();
        wrong["reply_pcm"][field] = json!(0);
        assert!(oracle::check(&wrong).is_err(), "zero {field}");
    }
    for count in [0, 9, 11] {
        let mut wrong = valid.clone();
        wrong["trace"][2]["frames"] = json!(count);
        wrong["trace"][2]["received_frames"] = json!(count);
        assert!(oracle::check(&wrong).is_err(), "wrong frame count {count}");
    }
}

pub(super) fn options() -> crate::lab_run::run_options::Options {
    let executable = std::env::current_exe().unwrap();
    let binary = executable
        .ancestors()
        .find(|path| matches!(path.file_name().and_then(|name| name.to_str()), Some("deps" | "build")))
        .and_then(std::path::Path::parent)
        .unwrap()
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

pub(super) fn utterance() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/lab/w1-utterance.pcm")
}

pub(super) async fn close_world(world: &mut super::super::world::World) -> Result<(), String> {
    use super::super::process::owners::Completion;
    let host = world
        .host
        .as_mut()
        .map(super::super::process::Process::observe_collection);
    let bridge = world
        .bridge
        .as_mut()
        .map(super::super::process::Process::observe_collection);
    let mut owned = std::mem::take(world);
    let mut completion = Completion::new("M2 bridge/host/Live/spoof", async move {
        let result = owned.close().await;
        (owned, result)
    });
    completion.stop();
    let (owned, result) = completion.assert_closed().await;
    *world = owned;
    for receipt in [host, bridge].into_iter().flatten() {
        assert!(receipt.try_recv().unwrap().success());
    }
    result
}

pub(super) async fn close_auth(auth: auth::Auth) {
    let mut completion = super::super::process::owners::Completion::new("M2 auth", auth.close());
    completion.stop();
    completion.assert_closed().await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production bridge and pinned hassmic-host"]
async fn m2_t3_concurrent_cases_are_independent_and_reap_all_endpoints() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let first_host = super::super::process::owners::Host::new();
    let second_host = super::super::process::owners::Host::new();
    let (mut first_options, mut second_options) = (options(), options());
    first_options.python = first_host.python().into();
    second_options.python = second_host.python().into();
    let utterance = utterance();
    let ((success, a), (expired, b)) = tokio::join!(
        journey::run(&first_options, first.path(), "success", &utterance),
        journey::run(&second_options, second.path(), "expired", &utterance)
    );
    assert_eq!(a, Ok(()));
    assert_eq!(b, Ok(()));
    assert_eq!(success["calls"], 1);
    assert_eq!(expired["calls"], 0);
    assert_eq!(expired["refreshes"], oracle::EXPIRED_REFRESHES);
    assert_eq!(std::fs::read_dir(first.path()).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(second.path()).unwrap().count(), 0);
    first_host.assert_closed();
    let rejected_host = std::path::Path::new(second_host.python())
        .parent()
        .unwrap()
        .join("host.json");
    assert!(!rejected_host.exists(), "rejected credential started a hassmic host");
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production bridge and pinned hassmic-host"]
async fn m2_t3_startup_failure_and_input_error_retain_partial_evidence_and_reap() {
    for failure in ["startup", "input"] {
        let root = tempfile::tempdir().unwrap();
        let host = super::super::process::owners::Host::new();
        let mut options = options();
        options.python = host.python().into();
        if failure == "startup" {
            options.bridge = Some(root.path().join("missing-bridge"));
        }
        let (measured, result) = journey::run(&options, root.path(), "success", &root.path().join("missing-pcm")).await;
        assert!(result.is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        if failure == "input" {
            assert_eq!(measured["frames"].as_array().unwrap().len(), 1);
        }
        host.assert_closed();
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production bridge and pinned hassmic-host"]
async fn m2_t3_cancellation_after_wake_reaps_and_retains_observed_frames() {
    let root = tempfile::tempdir().unwrap();
    let cancellation = tempfile::tempdir().unwrap();
    let host = super::super::process::owners::Host::new();
    let mut options = options();
    options.python = host.python().into();
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
    host.assert_closed();
}
