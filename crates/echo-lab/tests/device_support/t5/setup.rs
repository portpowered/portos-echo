//! Setup component journeys use the retained M2 steps, not M4 latency proof.
use super::*;
use std::net::{Ipv4Addr, TcpListener, TcpStream};

fn nominate(session: &Session, endpoint: &str, setup: Value) -> PathBuf {
    let path = voice::with_budgets(session, json!({}));
    let mut scenario: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    scenario["setup"] = setup;
    fs::write(&path, scenario.to_string()).unwrap();
    let config_path = session.0.join("bridge.toml");
    let config = fs::read_to_string(&config_path)
        .unwrap()
        .replace("http://127.0.0.1:18080/mcp", endpoint);
    fs::write(config_path, config).unwrap();
    path
}

#[test]
fn t3_device_t5_spoof_setup_uses_nominated_endpoint_and_joins_listener() {
    for real in [false, true] {
        let reserved = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = reserved.local_addr().unwrap();
        let session = prepare(&json!({"spoof_probe":true}));
        let path = nominate(
            &session,
            &format!("http://{address}/mcp"),
            json!({"mcp":"spoof","backend":"client"}),
        );
        let original = fs::read_to_string(session.0.join("bridge.toml")).unwrap();
        drop(reserved);
        let output = voice::budget_command(&session, real, &path).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let response: Value =
            serde_json::from_slice(&fs::read(session.0.join("spoof-response.json")).unwrap()).unwrap();
        assert_eq!(response["result"]["content"][0]["text"], "synthetic setup witness");
        assert_eq!(response["result"]["isError"], false);
        let rows = observations(&session);
        let observed: Vec<_> = rows.iter().filter(|row| row["event"] == "device.spoof").collect();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0]["test_echo_calls"], 1);
        assert_eq!(observed[0]["negative_injected"], false);
        assert!(TcpStream::connect(address).is_err());
        assert_eq!(session.report()["result"], "pass");
        assert_eq!(fs::read_to_string(session.0.join("bridge.toml")).unwrap(), original);
        assert!(!session.0.join("artifacts/configured.private").exists());
        let launch: Value = serde_json::from_slice(&fs::read(session.0.join("launch.json")).unwrap()).unwrap();
        let received = echo_config::BridgeConfig::from_toml_str(launch["config"].as_str().unwrap()).unwrap();
        assert_eq!(
            received.mcp,
            echo_config::BridgeConfig::from_toml_str(&original).unwrap().mcp
        );
    }
}

#[test]
fn t3_device_t5_spoof_bind_failure_preserves_foreign_listener_and_starts_no_bridge() {
    let reserved = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = reserved.local_addr().unwrap();
    let session = prepare(&json!({}));
    let path = nominate(&session, &format!("http://{address}/mcp"), json!({"mcp":"spoof"}));
    let output = voice::budget_command(&session, true, &path).output().unwrap();
    failed(
        &output,
        "device: unmet prerequisite: nominated spoof MCP endpoint unavailable",
    );
    assert!(TcpStream::connect(address).is_ok());
    assert!(
        !session
            .operations()
            .iter()
            .any(|op| op == "bridge" || op == "hold" || op == "play")
    );
    assert_eq!(session.report()["result"], "fail");
    assert!(!session.0.join("artifacts/configured.private").exists());
}

#[test]
fn t3_device_t5_spoof_cleanup_retains_failed_screen_and_late_error_verdicts() {
    for fixture in [json!({"screen_fault":"auth"}), json!({"late_error":true})] {
        let reserved = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = reserved.local_addr().unwrap();
        let session = prepare(&fixture);
        let path = nominate(&session, &format!("http://{address}/mcp"), json!({"mcp":"spoof"}));
        drop(reserved);
        let output = voice::budget_command(&session, true, &path).output().unwrap();
        failed(
            &output,
            if fixture["late_error"] == true {
                "device: bridge errors observed"
            } else {
                "screen: authentication failed"
            },
        );
        assert_eq!(session.report()["result"], "fail");
        assert!(TcpStream::connect(address).is_err());
        assert!(!session.0.join("artifacts/configured.private").exists());
        let rows = observations(&session);
        assert_eq!(rows.iter().filter(|row| row["event"] == "device.spoof").count(), 1);
    }
}

#[test]
fn t3_device_t5_spoof_setup_refuses_unsupported_values_and_endpoints_before_effects() {
    for (endpoint, setup, error) in [
        (
            "http://127.0.0.1:18080/mcp",
            json!({"backend":"muse"}),
            "device: unsupported setup backend",
        ),
        (
            "http://127.0.0.1:18080/mcp",
            json!({"mcp":"portos-pilot"}),
            "device: unsupported setup mcp",
        ),
        (
            "http://localhost:18080/mcp",
            json!({"mcp":"spoof"}),
            "numeric-loopback spoof MCP endpoint",
        ),
        (
            "http://127.0.0.1:18080/other",
            json!({"mcp":"spoof"}),
            "numeric-loopback spoof MCP endpoint",
        ),
    ] {
        let session = prepare(&json!({}));
        let path = nominate(&session, endpoint, setup);
        let output = voice::budget_command(&session, true, &path).output().unwrap();
        failed(&output, error);
        assert_eq!(session.operations(), Vec::<String>::new());
        assert!(!session.0.join("artifacts").exists());
    }
}
