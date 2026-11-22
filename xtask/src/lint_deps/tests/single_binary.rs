use super::{check, diagnostic, graph};
use serde_json::{Value, json};

const R5: &str = "R5: subsystems cannot depend on other subsystems or services";
const R6: &str = "R6: services cannot depend on subsystems or other services";
const R7: &str = "R7: echo-core-api cannot depend on I/O crates or loop implementations";

fn expected(source: &str, target: &str, rule: &str) -> Vec<String> {
    vec![format!(
        "{source} -> {target} / {rule} (single-binary plan section 2.4)"
    )]
}

fn assert_rejected(source: &str, target: &str, kind: Value, rule: &str) {
    assert_eq!(
        check(&graph(source, &[(target, kind)])),
        Ok(expected(source, target, rule))
    );
}

#[test]
fn sb_subsystem_rejects_other_subsystem() {
    let violations = check(&graph("echo-sub-a", &[("echo-sub-b", Value::Null)])).unwrap();
    assert_eq!(violations, expected("echo-sub-a", "echo-sub-b", R5));
    println!("{}", violations[0]);
}

#[test]
fn sb_subsystem_rejects_service() {
    assert_rejected("echo-sub-a", "echo-svc-x", Value::Null, R5);
}

#[test]
fn sb_service_rejects_subsystem() {
    assert_rejected("echo-svc-x", "echo-sub-a", Value::Null, R6);
}

#[test]
fn sb_service_rejects_other_service() {
    assert_rejected("echo-svc-x", "echo-svc-y", Value::Null, R6);
}

#[test]
fn sb_core_api_rejects_tokio() {
    let mut input = graph("echo-core-api", &[("tokio", Value::Null)]);
    input["packages"].as_array_mut().unwrap().truncate(1);
    input["workspace_members"] = json!(["opaque-root"]);
    assert_eq!(check(&input), Ok(expected("echo-core-api", "tokio", R7)));
}

#[test]
fn sb_controller_composes_through_core_api() {
    let targets = [
        ("echo-sub-a", Value::Null),
        ("echo-sub-b", Value::Null),
        ("echo-svc-x", Value::Null),
        ("echo-svc-y", Value::Null),
        ("echo-core-api", Value::Null),
    ];
    let mut input = graph("echo-session-controller", &targets);
    let packages = input["packages"].as_array_mut().unwrap();
    let (api, controller_and_implementations) = packages.split_last_mut().unwrap();
    for package in &mut controller_and_implementations[1..] {
        package["dependencies"] = json!([{"name": "echo-core-api", "kind": null}]);
    }
    api["dependencies"] = json!([{"name": "serde", "kind": null}]);
    assert_eq!(check(&input), Ok(vec![]));
}

#[test]
fn sb_lint_dependency_kinds_and_self_exemption() {
    for kind in ["dev", "build"] {
        for (source, target, rule) in [
            ("echo-sub-a", "echo-sub-b", R5),
            ("echo-svc-x", "echo-svc-y", R6),
            ("echo-core-api", "tokio", R7),
        ] {
            assert_rejected(source, target, json!(kind), rule);
        }
    }
    for name in ["echo-sub-a", "echo-svc-x"] {
        for kind in [Value::Null, json!("dev"), json!("build")] {
            let mut input = graph(name, &[(name, kind)]);
            input["packages"][0]["dependencies"][0]["rename"] = json!("test_alias");
            assert_eq!(check(&input), Ok(vec![]), "{name}");
        }
    }
}

#[test]
fn sb_core_api_rejects_io_and_loop_implementations() {
    for target in [
        "axum",
        "hyper-util",
        "rtc",
        "tokio-rustls",
        "tokio-tungstenite",
        "echo-device",
        "echo-bridge",
        "echo-sub-a",
        "echo-svc-x",
    ] {
        assert_rejected("echo-core-api", target, Value::Null, R7);
    }
}

#[test]
fn legacy_rules_keep_normal_and_build_coverage_and_skip_dev() {
    for (source, target, rule) in [
        ("echo-config", "tokio", 0),
        ("echo-audio", "echo-mcp-client", 1),
        ("echo-agent-loop", "echo-agent-muse", 2),
        ("echo-bridge", "oauth2", 3),
    ] {
        for kind in [Value::Null, json!("build")] {
            assert_eq!(
                check(&graph(source, &[(target, kind)])),
                Ok(vec![diagnostic(source, target, rule)])
            );
        }
        assert_eq!(check(&graph(source, &[(target, json!("dev"))])), Ok(vec![]));
    }
}
