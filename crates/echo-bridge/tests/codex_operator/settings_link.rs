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
            let rows = trace(&path);
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
    let mut base = configured::config(fixture);
    base.home = Some(echo_config::HomeConfig {
        dir: fixture.directory.path().join("agent-home"),
        agent_tools: echo_config::AgentTools::default(),
    });
    base.mcp.server = vec![echo_config::McpServer {
        name: "local".into(),
        url: format!("{}/mcp", mock.base),
        auth: echo_config::McpAuth::PortosDeviceCode,
        bearer_file: None,
        allow_tools: vec![],
    }];
    base.mcp.client_id = "synthetic-cli".into();
    base.mcp.state_dir = Some(fixture.directory.path().join("grant"));
    base.mcp.operator_listen = ([127, 0, 0, 1], 0).into();
    echo_config::EffectiveConfig {
        config: base.clone(),
        base,
        overlay: echo_config::SettingsOverlay::default(),
        warning: None,
    }
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
