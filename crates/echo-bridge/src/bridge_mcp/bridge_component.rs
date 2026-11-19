//! T3 component boundary: actual auth and MCP HTTP clients against local controlled peers.
use super::*;
use echo_config::{McpAuth, McpServer};
use echo_mcp_auth::{AuthOptions, mock::Mock};
use echo_mcp_client::{McpClient, McpError, TokenProvider};
use echo_screen_contract::UiState;
use std::{future::Future, task::Poll, time::Duration};

async fn drive<F: Future>(future: F) -> F::Output {
    tokio::pin!(future);
    std::future::poll_fn(|cx| match future.as_mut().poll(cx) {
        Poll::Ready(value) => Poll::Ready(value),
        Poll::Pending => {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await
}

#[tokio::test(start_paused = true)]
async fn t3_grant_resource_401_latches_before_return_and_blocks_token_and_refresh() {
    drive(async {
        let mock = Mock::bind().await.unwrap();
        let directory = tempfile::tempdir().unwrap();
        let (mut owner, mut screen) = linked(&mock, directory.path()).await;
        let server = McpServer {
            name: "local".into(),
            url: format!("{}/mcp", mock.base),
            auth: McpAuth::PortosDeviceCode,
            bearer_file: None,
            allow_tools: vec!["test.echo".into()],
        };
        let token = Token::new(server.clone(), Some(owner.grant.clone()));
        assert!(
            token.token().await.unwrap().expose() == "access-1",
            "wrong synthetic access generation"
        );
        mock.state.lock().unwrap().revoked = true;
        let client = McpClient::new(server, token, Duration::from_secs(5)).unwrap();
        assert_eq!(client.list().await.err(), Some(McpError::HttpStatus(401)));
        assert!(owner.grant.blocked());
        assert_eq!(
            owner.grant.link_status(),
            echo_screen_contract::settings_v1::LinkStatusV1::LinkRequired
        );
        assert_eq!(
            screen.try_recv().unwrap(),
            UiState::Error {
                message: bridge_presentation::RELINK.into(),
                relink: true
            }
        );
        assert_eq!(client.list().await.err(), Some(McpError::TokenUnavailable));
        let base = "http://127.0.0.1";
        let response = reqwest::Client::new()
            .post(format!("{base}:{}/operator/mcp", owner.address.port()))
            .bearer_auth("synthetic-owner")
            .json(&serde_json::json!({"operation":"refresh"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 422);
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap(),
            serde_json::json!({"error":"relink_required"})
        );
        assert_eq!(
            screen.try_recv().unwrap(),
            UiState::Error {
                message: bridge_presentation::RELINK.into(),
                relink: true
            }
        );
        client_provider_rejection(&owner, directory.path());
        assert_blocked(&mock);
        owner.close().await;
    })
    .await;
}

fn client_provider_rejection(owner: &Runtime, directory: &std::path::Path) {
    // Repeated callbacks cannot duplicate the grant's first rejection event.
    owner.grant.unauthorized();
    let entries: Vec<serde_json::Value> = std::fs::read_to_string(directory.join("trace"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let rejected: Vec<_> = entries.iter().filter(|e| e["event"] == "mcp.relink_required").collect();
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0]["reason"], "unauthorized");
    assert_eq!(rejected[0]["provider_auth_mode"], "api_key");
}

async fn linked(mock: &Mock, directory: &std::path::Path) -> (Runtime, tokio::sync::mpsc::Receiver<UiState>) {
    let pairing_file = directory.join("pair");
    std::fs::write(&pairing_file, "synthetic-owner").unwrap();
    let settings = Settings {
        options: AuthOptions {
            resource_url: format!("{}/mcp", mock.base),
            client_id: "synthetic-cli".into(),
            scopes: vec!["endpoint:read".into()],
            storage_dir: directory.join("grant"),
            request_timeout: Duration::from_secs(5),
        },
        listen: ([127, 0, 0, 1], 0).into(),
        pairing_file,
    };
    let (send, screen) = tokio::sync::mpsc::channel(32);
    let trace = crate::bridge_trace::Trace::open(&directory.join("trace"), "api_key").unwrap();
    let owner = Runtime::open(settings, Presentation::new(Some(send)), trace, None)
        .await
        .unwrap();
    assert_eq!(
        owner.grant.link_status(),
        echo_screen_contract::settings_v1::LinkStatusV1::LinkRequired
    );
    let link = owner.grant.client.begin_link().await.unwrap();
    let grant = owner.grant.clone();
    let approval = tokio::spawn(async move { grant.client.await_approval(link).await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(5)).await;
    approval.await.unwrap().unwrap();
    let requests = mock.state.lock().unwrap().requests.len();
    for _ in 0..3 {
        assert_eq!(
            owner.grant.link_status(),
            echo_screen_contract::settings_v1::LinkStatusV1::Linked
        );
    }
    assert_eq!(mock.state.lock().unwrap().requests.len(), requests);
    (owner, screen)
}

fn assert_blocked(mock: &Mock) {
    let state = mock.state.lock().unwrap();
    assert_eq!(state.requests.iter().filter(|r| r.path == "/mcp").count(), 1);
    assert_eq!(
        state
            .requests
            .iter()
            .filter(|r| r.form.get("grant_type").is_some_and(|v| v == "refresh_token"))
            .count(),
        0
    );
    assert!(
        state
            .requests
            .iter()
            .filter(|r| r.path == "/mcp")
            .all(|r| !r.headers.contains_key("dpop"))
    );
}
