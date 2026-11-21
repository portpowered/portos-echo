use super::*;
use crate::bridge_mcp::{Presentation, Runtime, bridge_component::drive};
use echo_mcp_auth::mock::{Mock, metadata};
use serde_json::{Value, json};

const REGISTRATION: &str = "/custom/register";

async fn configured_owner(mock: &Mock, root: &Path) -> (Runtime, Prepared) {
    let mut config =
        BridgeConfig::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/bridge.echo.toml")).unwrap();
    assert_eq!(config.mcp.client_id, None);
    config.codex.ca_file = None;
    config.mcp.state_dir = Some(root.join("grant"));
    config.mcp.operator_listen = ([127, 0, 0, 1], 0).into();
    config.mcp.server[0].url = format!("{}/mcp", mock.base);
    config.screen.pairing_token_file = root.join("pair");
    std::fs::write(&config.screen.pairing_token_file, "synthetic-owner").unwrap();
    let trace = crate::bridge_trace::Trace::open(&root.join("trace"), "api_key").unwrap();
    let owner = Runtime::open(
        Settings::configured(&config).unwrap().unwrap(),
        Presentation::new(None),
        trace,
        None,
    )
    .await
    .unwrap();
    config.mcp.operator_listen = owner.address;
    let prepared = Prepared {
        settings: Settings::configured(&config).unwrap().unwrap(),
        operation: Operation::Link,
    };
    (owner, prepared)
}

fn registration_response(mock: &Mock, status: u16, body: Value) {
    let mut discover = metadata(&mock.base);
    discover["registration_endpoint"] = format!("{}{REGISTRATION}", mock.base).into();
    let mut state = mock.state.lock().unwrap();
    state.metadata = Some(discover);
    state.fault = Some((REGISTRATION.into(), status, body));
}

#[tokio::test(start_paused = true)]
async fn t3_portos_link_registers_default_client() {
    drive(async {
        let mock = Mock::bind().await.unwrap();
        registration_response(&mock, 201, json!({"client_id":"dcr_synthetic"}));
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let (mut owner, prepared) = configured_owner(&mock, root).await;
        assert_eq!(mock.state.lock().unwrap().requests.len(), 2);
        let (send, mut flushed) = tokio::sync::mpsc::unbounded_channel();
        let mut output = Probe(send, Vec::new());
        let mut operation = Box::pin(prepared.execute(&mut output));
        let prefix = format!("Verification URL: {}/approve\nUser code: TEST-CODE\n", mock.base);
        assert_guidance(operation.as_mut(), &mut flushed, &prefix).await;
        let registrations = mock
            .state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r.path == REGISTRATION)
            .count();
        assert_eq!(registrations, 1, "first default link must POST one registration");
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(operation.await, Ok(()));
        assert_eq!(output.1, format!("{prefix}MCP linked\n").as_bytes());
        let grant: Value = serde_json::from_slice(&std::fs::read(root.join("grant/grant.json")).unwrap()).unwrap();
        assert_eq!(grant["client_id"], "dcr_synthetic");
        owner.close().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn t3_registration_failures_are_redacted_cli_receipts_without_challenge() {
    drive(async {
        for (configured, expected, message) in [
            (
                false,
                AuthError::RegistrationUnavailable,
                "Port OS registration is unavailable. Set mcp.client_id to use a registered client.",
            ),
            (
                true,
                AuthError::RegistrationRefused,
                "Port OS refused client registration (invalid_client_metadata).",
            ),
        ] {
            let mock = Mock::bind().await.unwrap();
            if configured {
                registration_response(
                    &mock,
                    400,
                    json!({"error":"invalid_client_metadata","error_description":"private detail"}),
                );
            }
            let directory = tempfile::tempdir().unwrap();
            let (mut owner, prepared) = configured_owner(&mock, directory.path()).await;
            let mut output = Vec::new();
            let error = prepared.execute(&mut output).await.unwrap_err();
            assert_eq!(error.to_string(), message);
            assert_eq!(error, Error::Mcp(expected));
            assert_eq!(output, [] as [u8; 0]);
            assert!(!owner.grant.client.has_grant());
            assert!(!directory.path().join("grant/grant.json").exists());
            assert_eq!(
                mock.state.lock().unwrap().requests.len(),
                if configured { 3 } else { 2 }
            );
            assert_link_failure(directory.path(), &error, configured);
            owner.close().await;
        }
    })
    .await;
}

// The trace names the failed step, its HTTP status and the OAuth code, and nothing else.
fn assert_link_failure(root: &Path, error: &Error, replied: bool) {
    let Error::Mcp(error) = error else {
        panic!("a link failure is an MCP error");
    };
    let text = std::fs::read_to_string(root.join("trace")).unwrap();
    assert!(!text.contains("private detail") && !text.contains("127.0.0.1"));
    let entries: Vec<Value> = text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    let events: Vec<_> = entries.iter().map(|entry| entry["event"].as_str().unwrap()).collect();
    assert_eq!(events, ["mcp.link.started", "mcp.link.failed"]);
    let mut failed = entries[1].as_object().unwrap().clone();
    assert!(failed.remove("elapsed_ms").is_some_and(|elapsed| elapsed.is_u64()));
    let expected = if replied {
        json!({"step":"registration","http_status":400,"oauth_error":"invalid_client_metadata"})
    } else {
        json!({"step":null,"http_status":null,"oauth_error":null})
    };
    let mut expected = expected.as_object().unwrap().clone();
    expected.insert("event".into(), "mcp.link.failed".into());
    expected.insert("provider_auth_mode".into(), "api_key".into());
    expected.insert("operation_id".into(), 1.into());
    expected.insert("category".into(), crate::bridge_mcp::category(error).into());
    assert_eq!(failed, expected);
}
