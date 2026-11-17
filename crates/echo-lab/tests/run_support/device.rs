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
        Some(original.replace("127.0.0.1", "localhost")),
        Some(original.replace("http://127.0.0.1:18080/mcp", "https://mcp.example.invalid/mcp")),
        Some(original.replace("auth = \"none\"", "auth = \"bearer_file\"")),
        Some(format!("{original}\n[muse]\n")),
    ] {
        let session = Session::new(&json!({}));
        let expected = if template.is_none() {
            "echo-lab: device: --bridge-config requires a readable TOML file\n"
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
                if cfg!(windows) { "python" } else { "python3" },
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
