use super::*;
use serde_json::{Value, json};

const REGISTER: &str = "/custom/register";
const EPOCH: u64 = 1_800_000_000;
const LEGACY_CLI: &str = "port1/systems/zero/oauth-clients/command-line-interface";

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
async fn t3_dynamic_grant_rotates_before_bearer_use_and_keeps_its_binding_after_reopen() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    offer_registration(&mock);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("grant.json");
    let client = automatic(&mock, directory.path(), EPOCH).await;
    let first_key = approve(&mock, client.clone(), &path).await;
    assert_rotated_bearer(&mock, &client, &path, "dcr_synthetic", &first_key, 2).await;
    drop(client);

    let reopened = automatic(&mock, directory.path(), EPOCH + 5).await;
    assert_eq!(drive(reopened.access_token()).await.unwrap().expose(), "access-2");
    assert_rotated_bearer(&mock, &reopened, &path, "dcr_synthetic", &first_key, 3).await;
    assert_eq!(mock.count(REGISTER), 1);
    assert_eq!(mock.state.lock().unwrap().jtis.len(), 4);
}

#[tokio::test(start_paused = true)]
async fn t3_legacy_grant_survives_refresh_and_denial_then_switches_on_approved_relink() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    offer_registration(&mock);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("grant.json");
    link_legacy(&mock, directory.path()).await;
    let original = stored(&path);
    assert_eq!(original["client_id"], LEGACY_CLI);

    let client = automatic(&mock, directory.path(), EPOCH + 5).await;
    let key = &original["private_key_jwk"];
    assert_rotated_bearer(&mock, &client, &path, LEGACY_CLI, key, 2).await;
    assert_eq!(mock.count(REGISTER), 0);

    let previous = std::fs::read(&path).unwrap();
    mock.state.lock().unwrap().polling.push_back("access_denied".into());
    let link = drive(client.begin_link()).await.unwrap();
    let task = start_approval(client.clone(), link).await;
    advance(Duration::from_secs(5)).await;
    error(drive(task).await.unwrap(), AuthError::AccessDenied);
    assert_eq!(std::fs::read(&path).unwrap(), previous);
    assert_eq!(drive(client.access_token()).await.unwrap().expose(), "access-2");

    let link = drive(client.begin_link()).await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), previous);
    let task = start_approval(client.clone(), link).await;
    advance(Duration::from_secs(5)).await;
    drive(task).await.unwrap().unwrap();
    let current = stored(&path);
    assert_eq!(current["client_id"], "dcr_synthetic");
    assert_eq!(current["private_key_jwk"], *key);
    assert_eq!(current["grant"]["refresh_token"], "refresh-3");
    assert_eq!(mock.count(REGISTER), 2);
    assert_eq!(mock.state.lock().unwrap().jtis.len(), 7);
}

#[tokio::test(start_paused = true)]
async fn t3_automatic_mode_revokes_with_the_committed_legacy_client() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    offer_registration(&mock);
    let directory = tempfile::tempdir().unwrap();
    link_legacy(&mock, directory.path()).await;
    let client = automatic(&mock, directory.path(), EPOCH + 5).await;
    drive(client.revoke()).await.unwrap();
    assert_eq!(mock.count(REGISTER), 0);
    assert_eq!(stored(&directory.path().join("grant.json"))["grant"], Value::Null);
    let state = mock.state.lock().unwrap();
    let revoke = state.requests.iter().find(|r| r.path == "/custom/end").unwrap();
    assert_eq!(
        revoke.form,
        [("client_id", LEGACY_CLI), ("token", "refresh-1")]
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect()
    );
    assert!(!revoke.headers.contains_key("dpop"));
}

fn offer_registration(mock: &Mock) {
    advertise(mock, json!(format!("{}{REGISTER}", mock.base)));
    mock.fault(REGISTER, 201, json!({"client_id":"dcr_synthetic"}));
}

async fn link_legacy(mock: &Mock, directory: &std::path::Path) {
    let mut explicit = options(mock, directory);
    explicit.client_id = LEGACY_CLI.into();
    let client = Arc::new(drive(AuthClient::open_at_epoch(explicit, EPOCH)).await.unwrap());
    let link = drive(client.begin_link()).await.unwrap();
    let task = start_approval(client, link).await;
    advance(Duration::from_secs(5)).await;
    drive(task).await.unwrap().unwrap();
}

fn stored(path: &std::path::Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

async fn assert_rotated_bearer(
    mock: &Mock,
    client: &AuthClient,
    path: &std::path::Path,
    id: &str,
    key: &Value,
    generation: u32,
) {
    let access = drive(client.force_refresh()).await.unwrap();
    assert_eq!(access.expose(), &format!("access-{generation}"));
    let record = stored(path);
    assert_eq!(record["client_id"], id);
    assert_eq!(record["resource"], format!("{}/mcp", mock.base));
    assert_eq!(record["private_key_jwk"], *key);
    assert_eq!(record["grant"]["access_token"], *access.expose());
    assert_eq!(record["grant"]["refresh_token"], format!("refresh-{generation}"));
    assert_eq!(drive(mock.resource(access.expose())).await, 200);
    let state = mock.state.lock().unwrap();
    let requests = &state.requests;
    let refresh = &requests[requests.len() - 2];
    assert_eq!(refresh.path, "/custom/token");
    assert_eq!(
        refresh.form,
        [
            ("grant_type", "refresh_token".into()),
            ("refresh_token", format!("refresh-{}", generation - 1)),
            ("client_id", id.into()),
            ("resource", format!("{}/mcp", mock.base)),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value))
        .collect()
    );
    let mcp = requests.last().unwrap();
    assert_eq!(mcp.path, "/mcp");
    assert_eq!(mcp.headers["authorization"], format!("Bearer {}", access.expose()));
    assert!(!mcp.headers.contains_key("dpop"));
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

#[tokio::test(start_paused = true)]
async fn t3_link_failure_names_only_the_step_status_and_oauth_code() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    advertise(&mock, json!(format!("{}{REGISTER}", mock.base)));
    let directory = tempfile::tempdir().unwrap();
    let client = automatic(&mock, directory.path(), EPOCH).await;
    assert_eq!(client.take_link_failure(), None);
    let private = json!("private detail");
    for (path, step, status, code, expected, seen) in [
        (REGISTER, "registration", 400, json!("invalid_redirect_uri"), AuthError::Protocol, Some("invalid_redirect_uri")),
        (REGISTER, "registration", 400, json!("invalid_client_metadata"), AuthError::RegistrationRefused, Some("invalid_client_metadata")),
        (REGISTER, "registration", 400, json!("Bearer synthetic.secret"), AuthError::Protocol, Some("unrecognized")),
        (REGISTER, "registration", 429, private.clone(), AuthError::Transport, None),
        (REGISTER, "registration", 201, json!(7), AuthError::Protocol, None),
    ] {
        mock.fault(path, status, json!({"error":code,"client_id":" ","error_description":"private detail"}));
        error(drive(client.begin_link()).await, expected);
        let failure = LinkFailure {
            step,
            http_status: Some(status),
            oauth_error: seen.map(str::to_owned),
        };
        assert_eq!(client.take_link_failure(), Some(failure));
        assert_eq!(client.take_link_failure(), None, "a failure is reported once");
    }
    assert_eq!(mock.count(REGISTER), 5);
    assert_eq!(mock.count("/custom/link"), 0);
}

#[tokio::test(start_paused = true)]
async fn t3_device_authorization_and_token_refusals_name_their_own_step() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    let directory = tempfile::tempdir().unwrap();
    let explicit = options(&mock, directory.path());
    let client = Arc::new(drive(AuthClient::open_at_epoch(explicit, EPOCH)).await.unwrap());
    mock.state.lock().unwrap().polling.push_back("invalid_client".into());
    let link = drive(client.begin_link()).await.unwrap();
    assert_eq!(client.take_link_failure(), None);
    let task = start_approval(client.clone(), link).await;
    advance(Duration::from_secs(5)).await;
    error(drive(task).await.unwrap(), AuthError::Protocol);
    let token = LinkFailure {
        step: "token",
        http_status: Some(400),
        oauth_error: Some("invalid_client".into()),
    };
    assert_eq!(client.take_link_failure(), Some(token));

    mock.fault("/custom/link", 400, json!({"error":"invalid_dpop_proof","error_description":"private detail"}));
    error(drive(client.begin_link()).await, AuthError::Protocol);
    let device = LinkFailure {
        step: "device_authorization",
        http_status: Some(400),
        oauth_error: Some("invalid_dpop_proof".into()),
    };
    assert_eq!(client.take_link_failure(), Some(device));
    error(drive(client.begin_link()).await, AuthError::Protocol);
    mock.state.lock().unwrap().fault = None;
    drive(client.begin_link()).await.unwrap();
    assert_eq!(client.take_link_failure(), None, "a new attempt clears the last failure");
    assert!(!client.has_grant());
}
