//! T3 component boundary: actual auth and MCP HTTP clients against local controlled peers.
use super::*;
use echo_config::{McpAuth, McpServer};
use echo_mcp_auth::{AuthOptions, LinkClient, mock::Mock};
use echo_mcp_client::{McpClient, McpError, TokenProvider};
use echo_screen_contract::UiState;
use std::{future::Future, task::Poll, time::Duration};

pub(crate) async fn drive<F: Future>(future: F) -> F::Output {
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
        ca_pem: None,
        options: AuthOptions {
            resource_url: format!("{}/mcp", mock.base),
            client_id: LinkClient::Explicit("synthetic-cli".into()),
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

/// T3 local HTTPS peer. Private certificates are generated per test, never committed.
pub(crate) struct TlsPeer {
    pub directory: tempfile::TempDir,
    child: tokio::process::Child,
    output: tokio::io::BufReader<tokio::process::ChildStdout>,
    pub config: echo_config::BridgeConfig,
}

impl TlsPeer {
    pub async fn start(wrong_hostname: bool) -> Self {
        Self::with_scopes(wrong_hostname, vec!["endpoint:read".into()]).await
    }

    pub async fn with_scopes(wrong_hostname: bool, scopes: Vec<String>) -> Self {
        Self::start_with_catalog(wrong_hostname, scopes, false).await
    }

    pub async fn start_with_catalog(wrong_hostname: bool, scopes: Vec<String>, full_catalog: bool) -> Self {
        use tokio::io::AsyncBufReadExt;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        certificate(root, wrong_hostname);
        let mut child = tokio::process::Command::new("python3")
            .args([
                "-u",
                "-c",
                TLS_PEER,
                &scopes.join(" "),
                if full_catalog { "full" } else { "query" },
            ])
            .current_dir(root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut output = tokio::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        output.read_line(&mut line).await.unwrap();
        let port: u16 = line.trim().parse().unwrap();
        let mut config =
            echo_config::BridgeConfig::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/bridge.echo.toml"))
                .unwrap();
        let origin = "https://127.0.0.1";
        config.mcp.server[0].url = format!("{origin}:{port}/mcp");
        config.mcp.client_id = Some("synthetic-cli".into());
        config.mcp.scopes = scopes;
        config.mcp.state_dir = Some(root.join("grant"));
        config.mcp.operator_listen = ([127, 0, 0, 1], 0).into();
        config.codex.ca_file = Some(root.join("ca.pem"));
        config.screen.pairing_token_file = root.join("pair");
        std::fs::write(&config.screen.pairing_token_file, "synthetic-owner").unwrap();
        config.audit.log_dir = root.join("audit");
        Self {
            directory,
            child,
            output,
            config,
        }
    }

    pub async fn finish(mut self) -> serde_json::Value {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        self.child.stdin.take().unwrap().write_all(b"stop\n").await.unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).await.unwrap();
        assert!(self.child.wait().await.unwrap().success());
        serde_json::from_str(&line).unwrap()
    }

    pub async fn pending(&mut self) {
        use tokio::io::AsyncBufReadExt;
        let mut line = String::new();
        self.output.read_line(&mut line).await.unwrap();
        assert_eq!(line.trim(), "pending");
    }

    pub async fn release(&mut self) {
        use tokio::io::AsyncWriteExt;
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"release\n")
            .await
            .unwrap();
    }
}

fn certificate(root: &std::path::Path, wrong_hostname: bool) {
    let san = if wrong_hostname {
        "DNS:unrelated.invalid"
    } else {
        "IP:127.0.0.1"
    };
    let cert = std::process::Command::new("openssl")
        .current_dir(root)
        .args([
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=synthetic",
            "-addext",
            &format!("subjectAltName={san}"),
            "-addext",
            "basicConstraints=critical,CA:FALSE",
            "-addext",
            "extendedKeyUsage=serverAuth",
            "-keyout",
            "key.pem",
            "-out",
            "ca.pem",
        ])
        .output()
        .unwrap();
    assert!(cert.status.success(), "{}", String::from_utf8_lossy(&cert.stderr));
}

const TLS_PEER: &str = include_str!("bridge_component/tls_peer.py");

pub(crate) async fn approve(owner: &Runtime) {
    let link = owner.grant.client.begin_link().await.unwrap();
    assert_eq!(link.user_code, "TEST-CODE");
    let grant = owner.grant.clone();
    let task = tokio::spawn(async move { grant.client.await_approval(link).await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    task.await.unwrap().unwrap();
}

#[cfg(unix)]
pub(crate) fn assert_private(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(
        std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

pub(crate) async fn assert_mcp_trust_refused(owner: &Runtime, config: &echo_config::BridgeConfig) {
    let unrelated = TlsPeer::start(false).await;
    let wrong_host = TlsPeer::start(true).await;
    let roots = std::fs::read(unrelated.config.codex.ca_file.as_ref().unwrap()).unwrap();
    let grant = owner.grant.clone();
    let server = config.mcp.server[0].clone();
    for pem in [None, Some(roots.as_slice())] {
        let host = McpClient::with_ca(
            server.clone(),
            Token::new(server.clone(), Some(grant.clone())),
            Duration::from_secs(5),
            pem,
        )
        .unwrap();
        assert_eq!(host.list().await.err(), Some(McpError::Transport));
    }
    for pem in [
        b"".as_slice(),
        b"not PEM",
        b"-----BEGIN CERTIFICATE-----\nbad\n-----END CERTIFICATE-----",
    ] {
        let host = McpClient::with_ca(
            server.clone(),
            Token::new(server.clone(), Some(grant.clone())),
            Duration::from_secs(5),
            Some(pem),
        );
        assert_eq!(host.err(), Some(McpError::InvalidConfiguration));
    }
    let server = wrong_host.config.mcp.server[0].clone();
    let pem = std::fs::read(wrong_host.config.codex.ca_file.as_ref().unwrap()).unwrap();
    let host = McpClient::with_ca(
        server.clone(),
        Token::new(server, Some(grant)),
        Duration::from_secs(5),
        Some(&pem),
    )
    .unwrap();
    assert_eq!(host.list().await.err(), Some(McpError::Transport));
    let zero = serde_json::json!({"link":0,"token":0,"catalog":0,"query":0});
    assert_eq!(unrelated.finish().await, zero);
    assert_eq!(wrong_host.finish().await, zero);
}

pub(crate) fn assert_tool_receipts(path: &std::path::Path, attempts: &[&str], completed: &[(u64, &str)]) {
    use serde_json::{Value, json};
    let raw = std::fs::read_to_string(path).unwrap();
    assert_eq!(raw.matches("synthetic-query-result").count(), 0);
    let rows: Vec<Value> = raw.lines().map(|row| serde_json::from_str(row).unwrap()).collect();
    let mut receipts: Vec<_> = rows
        .iter()
        .filter(|row| row["event"] == "mcp.tool.completed")
        .cloned()
        .collect();
    for receipt in &mut receipts {
        receipt["elapsed_ms"] = json!(0);
    }
    assert_eq!(
        receipts,
        completed
            .iter()
            .map(|(ordinal, outcome)| json!({
                "event":"mcp.tool.completed","server":"portos","name":"endpoint_query",
                "attempt_count":ordinal,"outcome":outcome,"elapsed_ms":0,"provider_auth_mode":"api_key"
            }))
            .collect::<Vec<_>>()
    );
    let calls: Vec<_> = rows
        .iter()
        .filter(|row| row["event"] == "tools/call")
        .map(|row| row["call_id"].clone())
        .collect();
    assert_eq!(calls, attempts.iter().map(|id| json!(id)).collect::<Vec<_>>());
    let mut catalogs: Vec<_> = rows
        .iter()
        .filter(|row| row["event"] == "mcp.tools_list.completed")
        .cloned()
        .collect();
    for catalog in &mut catalogs {
        catalog["elapsed_ms"] = json!(0);
    }
    assert_eq!(
        catalogs,
        vec![json!({"event":"mcp.tools_list.completed","server":"portos",
        "outcome":"success","count":1,"elapsed_ms":0,"provider_auth_mode":"api_key"})]
    );
}
