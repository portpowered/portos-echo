//! T3 CLI admission: controlled tools, no sockets, accounts or hardware.
use crate::run_support::{FAKE, Session, failed};
use serde_json::json;

#[test]
fn device_unsafe_or_unreadable_config_never_invokes_bench_tools() {
    let original = include_str!("../../../../examples/bridge.example.toml")
        .replace("echo.example.invalid", "127.0.0.1")
        .replace("0.0.0.0:16900", "127.0.0.1:0"); // lint-rules: allow-literal inert template substitution
    for template in [
        None,
        Some("malformed TOML".into()),
        Some(original.replace("max_tool_calls = 8", "max_tool_calls = 0")),
        Some(original.replace("max_tool_calls = 8", "unexpected_field = 8")),
        Some(original.replace("max_tool_calls = 8", "max_tool_calls = \"invalid\"")),
        Some(original.replace("host = \"127.0.0.1\"", "host = \"localhost\"")),
        Some(original.replace("http://127.0.0.1:18080/mcp", "https://mcp.example.invalid/mcp")), // lint-rules: allow-literal inert rejected config; no tool is invoked
        Some(original.replace("auth = \"none\"", "auth = \"bearer_file\"")),
        Some(format!("{original}\n[muse]\n")),
    ] {
        let session = Session::new(&json!({}));
        let expected = if template.is_none() {
            "echo-lab: device: --bridge-config requires a readable TOML file\n"
        } else if template.as_ref().is_some_and(|s| s.contains("host = \"localhost\"")) {
            "echo-lab: device: echo.host must be 127.0.0.1; forward the Echo's Wyoming port with adb forward\n"
        } else {
            "echo-lab: device: invalid or unsafe nominated bridge configuration\n"
        };
        if let Some(template) = template {
            std::fs::write(session.0.join("bridge.toml"), template).unwrap();
        }
        let output = session
            .command()
            .args([
                "run",
                "one.yaml",
                "--driver",
                "device",
                "--bench",
                "ring",
                "--port",
                FAKE,
                "--adb",
                FAKE,
                "--stt",
                FAKE,
                "--bridge",
                FAKE,
                "--bridge-config",
                "./bridge.toml",
                "--python",
                "python3",
                "--out",
                "artifacts",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(String::from_utf8_lossy(&output.stderr), expected);
        assert_eq!(session.operations(), Vec::<String>::new());
        assert!(!session.0.join("artifacts").exists());
    }
}

#[test]
fn wake_clip_sim_refusal_precedes_path_and_tool_resolution() {
    let session = Session::new(&json!({}));
    let result = session
        .command()
        .args(["run", "missing.yaml", "--driver", "sim", "--wake-clip", "missing.wav"])
        .output()
        .unwrap();
    failed(&result, "run: --wake-clip is only supported with --driver device");
    assert_eq!(
        String::from_utf8_lossy(&result.stderr),
        "echo-lab: run: --wake-clip is only supported with --driver device\n"
    );
    assert_eq!(session.operations(), Vec::<String>::new());
    assert!(!session.0.join("artifacts").exists());
}

#[test]
fn device_typed_ui_requires_pair_but_golden_dispatch_is_preserved() {
    let session = Session::new(&json!({}));
    session.scenario(
        "one.yaml",
        &json!([{"expect_screen":{"state":"ready","message":"Echo Live ready"}}]),
    );
    let path = session.0.join("one.yaml");
    let scenario = std::fs::read_to_string(&path).unwrap().replace("\"B0\"", "\"M1\"");
    std::fs::write(path, scenario).unwrap();
    let output = session.run(&[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("require --bridge and --bridge-config"));
    assert_eq!(session.operations(), Vec::<String>::new());
    assert!(!session.0.join("artifacts").exists());
}

fn configured(session: &Session, steps: &serde_json::Value) -> std::process::Command {
    let original = include_str!("../../../../examples/bridge.example.toml")
        .replace("echo.example.invalid", "127.0.0.1")
        .replace("0.0.0.0:16900", "127.0.0.1:0"); // lint-rules: allow-literal inert template substitution
    std::fs::write(session.0.join("bridge.toml"), original).unwrap();
    session.scenario("one.yaml", steps);
    let path = session.0.join("one.yaml");
    let scenario = std::fs::read_to_string(&path).unwrap().replace("\"B0\"", "\"M1\"");
    std::fs::write(path, scenario).unwrap();
    session.run_command(&["--bridge", FAKE, "--bridge-config", "bridge.toml"])
}

#[test]
fn device_schema_rejection_precedes_even_the_projection_child() {
    for change in ["max_tool_calls = 0", "max_tool_calls = \"invalid\"", "unknown = 8"] {
        let session = Session::new(&json!({}));
        let mut command = configured(&session, &json!([{ "say":"utterance.wav" }]));
        let path = session.0.join("bridge.toml");
        let contents = std::fs::read_to_string(&path)
            .unwrap()
            .replace("max_tool_calls = 8", change);
        std::fs::write(path, contents).unwrap();
        failed(
            &command.output().unwrap(),
            "device: invalid or unsafe nominated bridge configuration",
        );
        // Every nominated executable, including the projection helper, writes
        // this ledger. Rejection must happen before any of them is launched.
        assert_eq!(session.operations(), Vec::<String>::new());
        assert!(!session.0.join("artifacts").exists());
    }
}

#[test]
fn device_screen_faults_fail_closed_without_playback_and_collect_resources() {
    for (fault, message) in [
        ("auth", "screen: authentication failed"),
        ("eof", "screen: stream ended"),
        ("malformed", "screen: malformed presentation"),
        ("oversized", "screen: oversized line"),
    ] {
        let session = Session::new(&json!({"real_config":true,"device_bridge_fault":fault}));
        let output = configured(
            &session,
            &json!([
                {"expect_screen":{"state":"ready","message":"Echo Live ready"}},
                {"say":"utterance.wav"}
            ]),
        )
        .output()
        .unwrap();
        failed(&output, message);
        assert_eq!(session.report()["result"], "fail");
        assert_eq!(session.report()["checks"]["step_0"], false);
        assert!(session.report()["checks"]["step_1"].is_null());
        assert_eq!(session.report()["checks"]["session_released"], true);
        assert_eq!(session.operations().iter().filter(|op| *op == "play").count(), 0);
        assert!(!session.0.join("artifacts/configured.private").exists());
        assert!(!session.0.join("artifacts/bridge.private.jsonl").exists());
    }
}

#[test]
fn device_final_trace_rechecks_late_duplicates_incomplete_records_and_errors() {
    for (fault, message) in [
        ("duplicate", "expected 1 occurrences, observed 2"),
        ("incomplete", "trace: incomplete final JSON line"),
        ("error", "device: bridge errors observed"),
    ] {
        let session = Session::new(&json!({"real_config":true,"device_bridge_fault":fault}));
        let output = configured(
            &session,
            &json!([
                {"expect_trace":["wyoming.connected"]}
            ]),
        )
        .output()
        .unwrap();
        failed(&output, message);
        let report = session.report();
        assert_eq!(report["result"], "fail");
        assert_eq!(report["checks"]["session_released"], false);
        assert_eq!(report["checks"]["step_0"], fault == "error");
        assert_eq!(
            report["checks"]["bridge_errors_absent"],
            match fault {
                "duplicate" => json!(true),
                "error" => json!(false),
                _ => serde_json::Value::Null,
            }
        );
        assert!(!session.0.join("artifacts/configured.private").exists());
        assert!(!session.0.join("artifacts/bridge.private.jsonl").exists());
    }
}

#[test]
fn device_wake_never_uses_stale_run_or_hides_late_duplicate_without_explicit_trace_step() {
    for (fault, message) in [
        ("late_wake", "expected 1 occurrences, observed 2"),
        ("stale_wake", "wyoming.run_started): expected 1 occurrences, observed 0"),
    ] {
        let session = Session::new(&json!({"real_config":true,"device_bridge_fault":fault}));
        let output = configured(&session, &json!([{"say":"utterance.wav"}]))
            .output()
            .unwrap();
        failed(&output, message);
        let report = session.report();
        assert_eq!(report["result"], "fail");
        assert_eq!(report["checks"]["step_0"], false);
        assert_eq!(report["checks"]["session_released"], fault == "stale_wake");
        assert_eq!(session.operations().iter().filter(|op| *op == "play").count(), 1);
        assert!(!session.0.join("artifacts/configured.private").exists());
    }
}

fn wake_wav(rate: u32, samples: usize) -> Vec<u8> {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend(u32::try_from(samples * 2 + 36).unwrap().to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(rate.to_le_bytes());
    bytes.extend((rate * 2).to_le_bytes());
    bytes.extend(2_u16.to_le_bytes());
    bytes.extend(16_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(u32::try_from(samples * 2).unwrap().to_le_bytes());
    bytes.resize(samples * 2 + 44, 0);
    bytes
}

#[test]
fn wake_clip_admission_rejects_invalid_wav_before_bench_effects() {
    // Detailed format and duration branches use direct calls in device/clip_tests.
    let session = Session::new(&json!({"real_config":true}));
    std::fs::write(session.0.join("wake.wav"), b"not WAV").unwrap();
    let result = session.run(&["--wake-clip", "wake.wav"]);
    failed(
        &result,
        "device: invalid wake clip; expected 16-bit PCM mono WAV at 16000 or 48000 Hz, 0.5-10 s",
    );
    // The nominated Python helper is an offline preflight; no bench command runs.
    assert!(session.operations().iter().all(|op| op == "wake.wav"));
    assert!(!session.0.join("artifacts").exists());
}

#[test]
fn wake_clip_accepts_both_rates_without_mutating_input() {
    for (rate, samples) in [(16000, 16000), (48000, 48000)] {
        let session = Session::new(&json!({"real_config":true}));
        let bytes = wake_wav(rate, samples);
        std::fs::write(session.0.join("wake.wav"), &bytes).unwrap();
        let result = session.run(&["--wake-clip", "wake.wav", "--keep-audio"]);
        assert_eq!(
            result.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report = session.report();
        assert_eq!(report["m1"]["wake_clip"]["sample_rate_hz"], rate);
        if samples == usize::try_from(rate).unwrap() {
            let expected = if rate == 16000 {
                "643f8a8dc8bd9c19225afffad2becfec5426180b3749cb208abdf1a6c8354efc"
            } else {
                "0a8f76d89c709043814cb74f331a4578d17ff61256303bd0019a263d053f86e8"
            };
            assert_eq!(report["m1"]["wake_clip"]["sha256"], expected);
        }
        assert_eq!(
            report["m1"]["wake_clip"]["duration_ms"].as_f64().unwrap(),
            f64::from(u32::try_from(samples).unwrap()) * 1000.0 / f64::from(rate)
        );
        assert_eq!(report["m1"]["plays"].as_array().unwrap().len(), 1);
        assert_eq!(std::fs::read(session.0.join("wake.wav")).unwrap(), bytes);
        assert!(!session.0.join("artifacts/configured.private").exists());
        assert!(!report.to_string().contains("wake.wav"));
    }
}

#[test]
fn wake_clip_unreadable_and_repository_inputs_fail_closed() {
    for path in ["missing.wav", ".", env!("CARGO_MANIFEST_DIR")] {
        let session = Session::new(&json!({"real_config":true}));
        let result = session.run(&["--wake-clip", path]);
        failed(
            &result,
            "device: invalid wake clip; expected 16-bit PCM mono WAV at 16000 or 48000 Hz, 0.5-10 s",
        );
        assert!(!session.0.join("artifacts").exists());
        assert!(
            session
                .operations()
                .iter()
                .all(|op| !["firmware", "hold", "play"].contains(&op.as_str()))
        );
    }
}

#[test]
fn wake_clip_failures_keep_caller_input_and_remove_staging_with_audio_retention() {
    for fixture in [
        json!({"real_config":true,"hour":8}),
        json!({"real_config":true,"fail":"play"}),
        json!({"real_config":true,"cancel_on":"play"}),
    ] {
        let session = Session::new(&fixture);
        let bytes = wake_wav(16000, 16000);
        std::fs::write(session.0.join("wake.wav"), &bytes).unwrap();
        let result = session.run(&[
            "--wake-clip",
            "wake.wav",
            "--keep-audio",
            "--cancel-file",
            "cancel.signal",
        ]);
        assert_eq!(result.status.code(), Some(1));
        assert_eq!(std::fs::read(session.0.join("wake.wav")).unwrap(), bytes);
        assert!(!session.0.join("artifacts/configured.private").exists());
        assert_eq!(session.report()["m1"]["plays"], json!([]));
        if fixture["hour"] == 8 {
            failed(&result, "quiet hours: audio refused");
            assert!(session.operations().iter().all(|op| op != "play"));
        }
    }
}
