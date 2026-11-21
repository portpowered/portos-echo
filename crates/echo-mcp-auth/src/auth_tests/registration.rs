use super::*;
use serde_json::{Value, json};

const REGISTER: &str = "/custom/register";
const EPOCH: u64 = 1_800_000_000;

async fn automatic(mock: &Mock, directory: &std::path::Path, epoch: u64) -> Arc<AuthClient> {
    let explicit = options(mock, directory);
    let options = AuthOptions {
        resource_url: explicit.resource_url,
        client_id: LinkClient::Automatic {
            name: "Test Echo".into(),
        },
        scopes: vec!["endpoint:read".into(), "group:read".into()],
        storage_dir: explicit.storage_dir,
        request_timeout: explicit.request_timeout,
    };
    Arc::new(
        drive(AuthClient::open_configured(options, Some(epoch), None))
            .await
            .unwrap(),
    )
}

fn advertise(mock: &Mock, endpoint: Value) {
    let mut metadata = mock::metadata(&mock.base);
    metadata["registration_endpoint"] = endpoint;
    mock.state.lock().unwrap().metadata = Some(metadata);
}

#[tokio::test(start_paused = true)]
async fn t3_default_registration_commits_identity_and_reuses_it_after_reopen() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    advertise(&mock, json!(format!("{}{REGISTER}", mock.base)));
    mock.fault(REGISTER, 201, json!({"client_id":"dcr_synthetic","extension":true}));
    let directory = tempfile::tempdir().unwrap();
    let client = automatic(&mock, directory.path(), EPOCH).await;
    let path = directory.path().join("grant.json");
    assert!(!client.has_grant());
    assert_eq!(mock.count(REGISTER), 0);
    let key = approve(&mock, client, &path).await;
    mock.state.lock().unwrap().metadata = None;
    let next = automatic(&mock, directory.path(), EPOCH + 5).await;
    assert!(next.has_grant());
    assert_eq!(approve(&mock, next, &path).await, key);
    assert_eq!(mock.count(REGISTER), 1);
    assert_eq!(mock.count("/custom/link"), 2);
    assert_eq!(mock.count("/custom/token"), 2);
    assert_requests(&mock);
}

async fn approve(mock: &Mock, client: Arc<AuthClient>, path: &std::path::Path) -> Value {
    let before = std::fs::read(path).ok();
    let link = drive(client.begin_link()).await.unwrap();
    assert_eq!(link.user_code, "TEST-CODE");
    assert_eq!(std::fs::read(path).ok(), before);
    let task = start_approval(client.clone(), link).await;
    advance(Duration::from_secs(5)).await;
    drive(task).await.unwrap().unwrap();
    let record: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let schema: Value = serde_json::from_str(include_str!("../../data/auth-store.schema.json")).unwrap();
    assert!(jsonschema::validator_for(&schema).unwrap().is_valid(&record));
    assert!(client.has_grant());
    assert_eq!(record["client_id"], "dcr_synthetic");
    let exchanges = mock.count("/custom/token");
    assert_eq!(record["grant"]["refresh_token"], format!("refresh-{exchanges}"));
    record["private_key_jwk"].clone()
}

fn assert_requests(mock: &Mock) {
    let state = mock.state.lock().unwrap();
    let register = state.requests.iter().find(|request| request.path == REGISTER).unwrap();
    assert_eq!(register.method, "POST");
    assert_eq!(register.headers["content-type"], "application/json");
    for header in ["authorization", "dpop", "cookie"] {
        assert!(!register.headers.contains_key(header));
    }
    assert_eq!(
        register.json,
        Some(json!({
            "client_name":"Test Echo",
            "token_endpoint_auth_method":"none",
            "grant_types":[wire::DEVICE_GRANT,"refresh_token"],
            "scope":"endpoint:read group:read",
        }))
    );
    for request in state
        .requests
        .iter()
        .filter(|r| r.path == "/custom/link" || r.path == "/custom/token")
    {
        let expected = if request.path == "/custom/link" {
            [
                ("client_id", "dcr_synthetic".into()),
                ("scope", "endpoint:read group:read".into()),
                ("resource", format!("{}/mcp", mock.base)),
            ]
        } else {
            [
                ("client_id", "dcr_synthetic".into()),
                ("grant_type", wire::DEVICE_GRANT.into()),
                ("device_code", "synthetic-device".into()),
            ]
        };
        assert_eq!(request.form, expected.into_iter().map(|(k, v)| (k.into(), v)).collect());
    }
    assert_eq!(state.jtis.len(), 4); // The mock verifies signatures against one pinned public key.
}

#[tokio::test(start_paused = true)]
async fn t3_explicit_id_skips_registration_even_with_unusable_metadata() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    advertise(&mock, json!(42));
    let directory = tempfile::tempdir().unwrap();
    let client = Arc::new(
        drive(AuthClient::open_at_epoch(options(&mock, directory.path()), EPOCH))
            .await
            .unwrap(),
    );
    let link = drive(client.begin_link()).await.unwrap();
    let task = start_approval(client, link).await;
    advance(Duration::from_secs(5)).await;
    drive(task).await.unwrap().unwrap();
    let record: Value = serde_json::from_slice(&std::fs::read(directory.path().join("grant.json")).unwrap()).unwrap();
    assert_eq!(record["client_id"], "synthetic-cli");
    assert_eq!(mock.count(REGISTER), 0);
    assert_eq!(mock.count("/custom/link"), 1);
    assert_eq!(mock.count("/custom/token"), 1);
}

#[tokio::test(start_paused = true)]
async fn t3_registration_failures_never_issue_a_challenge_or_save_a_grant() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    let directory = tempfile::tempdir().unwrap();
    for endpoint in [Value::Null, json!("http://127.0.0.1:1/custom/register")] {
        advertise(&mock, endpoint);
        let client = automatic(&mock, directory.path(), EPOCH).await;
        error(drive(client.begin_link()).await, AuthError::RegistrationUnavailable);
    }
    assert_eq!(mock.count(REGISTER), 0);
    advertise(&mock, json!(format!("{}{REGISTER}", mock.base)));
    for (status, body, expected) in [
        (
            400,
            json!({"error":"invalid_client_metadata","error_description":"private detail"}),
            AuthError::RegistrationRefused,
        ),
        (429, json!({"error":"private detail"}), AuthError::Transport),
        (503, json!({"error":"private detail"}), AuthError::Transport),
        (201, json!({"client_id":" "}), AuthError::Protocol),
    ] {
        mock.fault(REGISTER, status, body);
        let client = automatic(&mock, directory.path(), EPOCH).await;
        error(drive(client.begin_link()).await, expected);
        assert!(!client.has_grant());
    }
    assert_eq!(mock.count(REGISTER), 4);
    assert_eq!(mock.count("/custom/link"), 0);
    assert_eq!(mock.count("/custom/token"), 0);
    assert!(!directory.path().join("grant.json").exists());
}
