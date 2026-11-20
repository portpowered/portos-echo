//! T3: real settings HTTP projects the same grant changed by operator HTTP.
use super::*;
use echo_bridge::{Error, bridge_voice::Configured};
use echo_link::fake::{FakeConnector, Observer};
use echo_mcp_auth::mock::Mock;
use std::{future::Future, task::Poll};

#[tokio::test(start_paused = true)]
#[ignore = "T3 controlled Python child and loopback OAuth/settings"]
async fn t3_settings_runtime_projects_link_unlink_without_token_acquisition() {
    drive(async {
        let fixture = Fixture::new("chatgpt");
        let mock = Mock::bind().await.unwrap();
        let startup = startup(&fixture, &mock);
        let path = fixture.directory.path().join("settings-link-trace");
        let prepared = Configured::from_effective(startup, path.clone(), ([127, 0, 0, 1], 0).into()).unwrap();
        let (stop, stopping) = tokio::sync::oneshot::channel();
        let work = prepared.execute_with_settings(
            FakeConnector::new(Observer::default()),
            async || -> Result<echo_media::Peer, Error> { unreachable!("no wake") },
            vec![],
            async {
                stopping.await.unwrap();
            },
        );
        let inspect = async {
            let _control = fixture.control().await;
            let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
            while bridge_wire_screen::state(&mut screen).await["message"] != "Echo Live ready" {}
            let client = reqwest::Client::new();
            snapshot(&client, &fixture, &mock, "link_required").await;
            let url = operator_url(&path);
            let approve = async {
                while bridge_wire_screen::state(&mut screen).await["state"] != "link_code" {}
                tokio::time::advance(Duration::from_secs(5)).await;
            };
            let ((), ()) = tokio::join!(operate(&client, &url, "link", "linked"), approve);
            snapshot(&client, &fixture, &mock, "linked").await;
            operate(&client, &url, "unlink", "unlinked").await;
            snapshot(&client, &fixture, &mock, "link_required").await;
            assert_eq!(mock.state.lock().unwrap().generation, 1);
            stop.send(()).unwrap();
        };
        let (result, ()) = Box::pin(async { tokio::join!(work, inspect) }).await;
        let completion = result.unwrap();
        assert!(!completion.restart_requested);
        assert!(completion.settings.is_some());
        super::host::released(&fixture).await;
        presentation_released(&path, "chatgpt", true);
    })
    .await;
}

fn startup(fixture: &Fixture, mock: &Mock) -> echo_config::EffectiveConfig {
    portos_startup(fixture, &mock.base, "synthetic-cli")
}

fn operator_url(path: &std::path::Path) -> reqwest::Url {
    let rows = trace(path);
    let row = rows
        .iter()
        .find(|row| row["event"] == "mcp.operator.listening")
        .unwrap();
    let address = row["address"]
        .as_str()
        .unwrap()
        .parse::<std::net::SocketAddr>()
        .unwrap();
    assert_eq!(address.ip(), std::net::Ipv4Addr::LOCALHOST);
    let mut url = reqwest::Url::parse("http://127.0.0.1").unwrap();
    url.set_port(Some(address.port())).unwrap();
    url.set_path("/operator/mcp");
    url
}

fn portos_startup(fixture: &Fixture, base_url: &str, client_id: &str) -> echo_config::EffectiveConfig {
    let mut base = configured::config(fixture);
    base.home = Some(echo_config::HomeConfig {
        dir: fixture.directory.path().join("agent-home"),
        agent_tools: echo_config::AgentTools::default(),
    });
    base.mcp.server = vec![echo_config::McpServer {
        name: "local".into(),
        url: format!("{base_url}/mcp"),
        auth: echo_config::McpAuth::PortosDeviceCode,
        bearer_file: None,
        allow_tools: vec![],
    }];
    base.mcp.client_id = client_id.into();
    base.mcp.state_dir = Some(fixture.directory.path().join("grant"));
    base.mcp.operator_listen = ([127, 0, 0, 1], 0).into();
    echo_config::EffectiveConfig {
        config: base.clone(),
        base,
        overlay: echo_config::SettingsOverlay::default(),
        warning: None,
    }
}

struct PairedGrant<'a> {
    fixture: &'a Fixture,
    mock: &'a echo_lab::lab_mcp::Fixture,
    client: reqwest::Client,
    captured: Vec<String>,
}

impl PairedGrant<'_> {
    fn post(&self, body: &Value) -> reqwest::RequestBuilder {
        self.client
            .post(configured::settings_runtime_url(self.fixture, "/settings/v2/portos"))
            .json(body)
    }

    async fn capture(&mut self, request: reqwest::RequestBuilder, status: u16) -> Value {
        let response = request.send().await.unwrap();
        assert_eq!(response.status().as_u16(), status);
        let text = response.text().await.unwrap();
        let body = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap()
        };
        self.captured.push(text);
        body
    }

    async fn portos(&mut self) -> Value {
        let url = configured::settings_runtime_url(self.fixture, "/settings/v2");
        self.capture(self.client.get(url), 200).await["portos"].clone()
    }

    async fn begin(&mut self, epoch: u64) {
        assert_eq!(
            self.capture(self.post(&json!({"version":2,"operation":"link"})), 202)
                .await,
            json!({"version":2,"operation":"link"})
        );
        loop {
            let status = self.portos().await;
            assert_eq!(status["operation_state"], "linking");
            let Some(challenge) = status.get("challenge") else {
                continue;
            };
            let expires = challenge["expires_at_unix"].as_u64().unwrap();
            assert!((epoch + 1800..=epoch + 1802).contains(&expires));
            assert_eq!(
                challenge,
                &json!({"user_code":"TEST-CODE","verification_uri":format!("{}/approve",self.mock.base()),
                    "expires_at_unix":expires})
            );
            writeln!(std::io::stdout().lock(), "paired Port OS challenge: {challenge}").unwrap();
            break;
        }
    }

    async fn outcome(&mut self, operation: &str, expected: &str) -> Value {
        loop {
            let status = self.portos().await;
            if status["operation_state"] != operation {
                assert_eq!(status["operation_state"], expected);
                assert_eq!(status.get("challenge"), None);
                return status;
            }
        }
    }
}

async fn paired_grant_journey(fixture: &Fixture, expected: &str) {
    use echo_lab::lab_mcp::Fixture as MockPortos;
    let epoch = std::time::UNIX_EPOCH.elapsed().unwrap().as_secs();
    let mock = MockPortos::bind(epoch).await.unwrap();
    let mut startup = portos_startup(fixture, mock.base(), "echo-lab");
    startup.base.mcp.scopes = vec!["endpoint:read".into()];
    startup.config = startup.base.clone();
    let path = fixture.directory.path().join("paired-portos-trace");
    let prepared = Configured::from_effective(startup, path.clone(), ([127, 0, 0, 1], 0).into()).unwrap();
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let work = prepared.execute_with_settings(
        FakeConnector::new(Observer::default()),
        async || -> Result<echo_media::Peer, Error> { unreachable!("no wake") },
        vec![],
        async { stopping.await.unwrap() },
    );
    let inspect = async {
        let _control = fixture.control().await;
        let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
        while bridge_wire_screen::state(&mut screen).await["message"] != "Echo Live ready" {}
        let mut journey = PairedGrant {
            fixture,
            mock: &mock,
            client: reqwest::Client::new(),
            captured: vec![],
        };
        if expected == "idle" {
            complete_link_unlink(&mut journey, &path, epoch).await;
        } else {
            failed_link(&mut journey, epoch, expected).await;
        }
        stop.send(()).unwrap();
    };
    let (result, ()) = Box::pin(async { tokio::join!(work, inspect) }).await;
    assert!(!result.unwrap().restart_requested);
    super::host::released(fixture).await;
    let reopened = echo_mcp_auth::AuthClient::open(echo_mcp_auth::AuthOptions {
        resource_url: format!("{}/mcp", mock.base()),
        client_id: "echo-lab".into(),
        scopes: vec!["endpoint:read".into()],
        storage_dir: fixture.directory.path().join("grant"),
        request_timeout: Duration::from_secs(5),
    })
    .await
    .unwrap();
    assert!(!reopened.has_grant());
    mock.shutdown().await.unwrap();
}

async fn failed_link(journey: &mut PairedGrant<'_>, epoch: u64, state: &str) {
    use echo_lab::lab_mcp::Approval;
    journey.begin(epoch).await;
    let seconds = if state == "denied" {
        journey.mock.approve(Approval::Denied).unwrap();
        5
    } else {
        1800
    };
    journey.mock.advance(seconds).unwrap();
    tokio::time::advance(Duration::from_secs(seconds)).await;
    let refused = journey.outcome("linking", state).await;
    assert_eq!(refused["link_status"], "link_required");
    journey.begin(epoch + seconds).await;
    assert_eq!(journey.mock.counts().unwrap().links, 2);
    writeln!(
        std::io::stdout().lock(),
        "Port OS {state}: challenge cleared; explicit retry admitted"
    )
    .unwrap();
}

async fn complete_link_unlink(journey: &mut PairedGrant<'_>, path: &std::path::Path, epoch: u64) {
    refuse_unpaired_link(journey).await;
    approve_paired_link(journey, path, epoch).await;
    let grant_path = journey.fixture.directory.path().join("grant/grant.json");
    let grant: Value = serde_json::from_slice(&std::fs::read(&grant_path).unwrap()).unwrap();
    unlink_paired_grant(journey).await;
    let stored: Value = serde_json::from_slice(&std::fs::read(&grant_path).unwrap()).unwrap();
    assert_eq!(stored["grant"], Value::Null);
    assert_private(journey, path, &grant);
    writeln!(
        std::io::stdout().lock(),
        "Port OS: scopes=[endpoint:read] links=1 revokes=1 shared_slot=409 credential_matches=0 grant_present=false"
    )
    .unwrap();
}

async fn refuse_unpaired_link(journey: &mut PairedGrant<'_>) {
    let status = journey.portos().await;
    assert_eq!(status["link_status"], "link_required");
    assert_eq!(status["server"]["requested_scopes"], json!(["endpoint:read"]));
    let link = json!({"version":2,"operation":"link"});
    journey.capture(journey.post(&link).header("Origin", "null"), 403).await;
    let url = configured::settings_runtime_url(journey.fixture, "/settings/v2/portos");
    journey
        .capture(journey.client.post(url).query(&[("token", "wrong")]).json(&link), 401)
        .await;
    assert_eq!(journey.mock.counts().unwrap().links, 0);
}

async fn approve_paired_link(journey: &mut PairedGrant<'_>, path: &std::path::Path, epoch: u64) {
    use echo_lab::lab_mcp::Approval;
    journey.begin(epoch).await;
    let duplicate = journey
        .capture(journey.post(&json!({"version":2,"operation":"link"})), 409)
        .await;
    assert_eq!(
        duplicate,
        json!({"version":2,"message":"A Port OS operation is already in progress."})
    );
    let cli = journey
        .client
        .post(operator_url(path))
        .bearer_auth("synthetic-screen-pair")
        .json(&json!({"operation":"unlink"}));
    journey.capture(cli, 409).await;
    journey.mock.approve(Approval::Approved).unwrap();
    journey.mock.advance(5).unwrap();
    tokio::time::advance(Duration::from_secs(5)).await;
    let linked = journey.outcome("linking", "idle").await;
    assert_eq!(linked["link_status"], "linked");
    assert_eq!(linked["server"]["granted_scopes"], json!(["endpoint:read"]));
}

async fn unlink_paired_grant(journey: &mut PairedGrant<'_>) {
    let unconfirmed = journey.post(&json!({"version":2,"operation":"unlink","confirm":false}));
    journey.capture(unconfirmed, 400).await;
    assert_eq!(journey.mock.counts().unwrap().revokes, 0);
    let request = journey.post(&json!({"version":2,"operation":"unlink","confirm":true}));
    assert_eq!(
        journey.capture(request, 202).await,
        json!({"version":2,"operation":"unlink"})
    );
    let unlinked = journey.outcome("unlinking", "idle").await;
    assert_eq!(unlinked["link_status"], "link_required");
    assert_eq!(unlinked["server"]["granted_scopes"], json!([]));
    let counts = journey.mock.counts().unwrap();
    assert_eq!((counts.links, counts.revokes), (1, 1));
}

fn assert_private(journey: &PairedGrant<'_>, path: &std::path::Path, stored: &Value) {
    let rows = std::fs::read_to_string(path).unwrap();
    for secret in [
        stored["grant"]["access_token"].as_str().unwrap(),
        stored["grant"]["refresh_token"].as_str().unwrap(),
        stored["private_key_jwk"]["d"].as_str().unwrap(),
        "synthetic-device-1",
    ] {
        assert!(!rows.contains(secret), "private grant material reached trace");
        assert!(
            journey.captured.iter().all(|body| !body.contains(secret)),
            "private grant material reached response"
        );
    }
    let operations = trace(path)
        .into_iter()
        .filter_map(|row| match row["event"].as_str()? {
            name @ ("mcp.link.started" | "mcp.link.approved" | "mcp.unlink.completed") => Some(name.to_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        operations,
        ["mcp.link.started", "mcp.link.approved", "mcp.unlink.completed"]
    );
}

#[tokio::test(start_paused = true)]
#[ignore = "T3 controlled Python child and echo-lab OAuth/settings HTTP"]
async fn t3_settings_v2_portos_shared_owner_and_private_grant() {
    drive(async { paired_grant_journey(&Fixture::new("chatgpt"), "idle").await }).await;
}

#[tokio::test(start_paused = true)]
#[ignore = "T3 controlled Python child and echo-lab OAuth/settings HTTP"]
async fn t3_settings_v2_portos_denial_clears_challenge() {
    drive(async { paired_grant_journey(&Fixture::new("chatgpt"), "denied").await }).await;
}

#[tokio::test(start_paused = true)]
#[ignore = "T3 controlled Python child and echo-lab OAuth/settings HTTP"]
async fn t3_settings_v2_portos_expiry_clears_challenge() {
    drive(async { paired_grant_journey(&Fixture::new("chatgpt"), "expired").await }).await;
}

async fn snapshot(client: &reqwest::Client, fixture: &Fixture, mock: &Mock, expected: &str) {
    let requests = mock.state.lock().unwrap().requests.len();
    let response = client
        .get(configured::settings_runtime_url(fixture, "/settings/v1"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let snapshot: Value = response.json().await.unwrap();
    assert_eq!(snapshot["link_status"], expected);
    assert_eq!(mock.state.lock().unwrap().requests.len(), requests);
}

async fn operate(client: &reqwest::Client, url: &reqwest::Url, operation: &str, expected: &str) {
    let response = client
        .post(url.clone())
        .bearer_auth("synthetic-screen-pair")
        .json(&json!({"operation":operation}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.json::<Value>().await.unwrap(), json!({"outcome":expected}));
}

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
