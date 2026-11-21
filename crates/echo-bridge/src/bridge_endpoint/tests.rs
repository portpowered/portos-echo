//! T3: Settings service, registered grants, REST/uplink and gateway over loopback.
use super::*;
use crate::{
    bridge_mcp,
    bridge_settings::{SettingsHost, SettingsService},
    bridge_trace::Trace,
};
use echo_config::{HomeConfig, McpAuth, McpServer, SettingsOverlay};
use echo_contracts::secret::Secret;
use echo_mcp_auth::{AuthOptions, LinkClient, mock::Mock};
use echo_screen_contract::settings_v1::PromptTextsV1;
use serde_json::{Value, json};
use std::{fs, path::Path};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::advance,
};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::handshake::server::{Request, Response},
};

const SETTINGS_TOKEN: &str = "synthetic-paired-settings";
const LINK: &str = "loopback link/%";
const ROUTE: &str = "resolved-fake-route";

struct Resolver;

impl RouteResolver for Resolver {
    fn resolve<'a>(&'a self, auth_link_id: &'a str, tokens: &'a dyn EndpointTokenSource) -> Resolve<'a> {
        Box::pin(async move {
            assert_eq!(auth_link_id, LINK);
            assert!(tokens.access_token().await.unwrap().expose() == "endpoint-access-1");
            Some(ROUTE.into())
        })
    }
}

struct Gateway {
    template: String,
    received: mpsc::Receiver<GatewayEvent>,
    task: JoinHandle<()>,
}

#[derive(Debug, PartialEq, Eq)]
enum GatewayEvent {
    Upgrade(String, String, bool),
    Disconnected,
}

impl Gateway {
    async fn open() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let template = format!("ws://{}/devices/{{authLinkId}}", listener.local_addr().unwrap());
        let (events, received) = mpsc::channel(8);
        let task = tokio::spawn(gateway(listener, events));
        Self {
            template,
            received,
            task,
        }
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn gateway(listener: TcpListener, events: mpsc::Sender<GatewayEvent>) {
    use futures_util::StreamExt;
    while let Ok((stream, _)) = listener.accept().await {
        let Ok(mut socket) = accept_hdr_async(stream, |request: &Request, response: Response| {
            let bearer = request
                .headers()
                .get("Authorization")
                .and_then(|header| header.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            events
                .try_send(GatewayEvent::Upgrade(
                    request.uri().path().into(),
                    bearer,
                    request.headers().contains_key("DPoP"),
                ))
                .unwrap();
            Ok(response)
        })
        .await
        else {
            continue;
        };
        while socket.next().await.is_some_and(|message| message.is_ok()) {}
        events.send(GatewayEvent::Disconnected).await.unwrap();
    }
}

async fn registered(mock: &Mock, home: &Path) -> bridge_mcp::Runtime {
    let register = "/synthetic-registration";
    {
        let mut state = mock.state.lock().unwrap();
        let mut metadata = echo_mcp_auth::mock::metadata(&mock.base);
        metadata["registration_endpoint"] = json!(format!("{}{register}", mock.base));
        state.metadata = Some(metadata);
        state.fault = Some((register.into(), 201, json!({"client_id":"synthetic-dcr"})));
    }
    let pairing = home.join("pair");
    fs::write(&pairing, SETTINGS_TOKEN).unwrap();
    let settings = bridge_mcp::Settings {
        options: AuthOptions {
            resource_url: format!("{}/mcp", mock.base),
            client_id: LinkClient::Automatic {
                name: "Echo Test".into(),
            },
            scopes: vec!["endpoint:read".into()],
            request_timeout: REQUEST_TIMEOUT,
            storage_dir: home.into(),
        },
        ca_pem: None,
        listen: ([127, 0, 0, 1], 0).into(),
        pairing_file: pairing,
    };
    let presentation = bridge_mcp::bridge_presentation::Presentation::new(None);
    let trace = Trace::open(&home.join("trace"), "api_key").unwrap();
    let owner = bridge_mcp::Runtime::open(settings, presentation, trace, None)
        .await
        .unwrap();
    let mut operation = owner
        .operator
        .start(bridge_mcp::bridge_operations::Operation::Link)
        .ok()
        .unwrap();
    let instructions = operation.progress.recv().await.unwrap();
    assert!(matches!(instructions, echo_screen_contract::UiState::LinkCode { .. }));
    advance(Duration::from_secs(5)).await;
    assert_eq!(operation.result.await.unwrap().unwrap(), "linked");
    owner
}

struct Settings {
    server: echo_screen_server::ScreenServer,
    actions: String,
    client: reqwest::Client,
    stop: oneshot::Sender<()>,
    owner: JoinHandle<()>,
    _states: mpsc::Sender<echo_screen_contract::UiState>,
}

impl Settings {
    async fn open(home: &Path, mock: &Mock, gateway: &Gateway, grant: Arc<Grant>) -> Self {
        let owner = settings_owner(home, mock);
        let (service, send) = SettingsService::resume(Box::new(owner));
        let runtime = Runtime::new(
            home.into(),
            Arc::new(Resolver),
            EndpointExchange::loopback(&mock.base, REQUEST_TIMEOUT).unwrap(),
            Transport::new(&gateway.template, &mock.base, true).unwrap(),
            &Default::default(),
        );
        let (stop, done) = oneshot::channel();
        let owner = tokio::spawn(async move {
            service
                .with_endpoint(runtime)
                .scope_with_grant(
                    async {
                        let _ = done.await;
                    },
                    || grant.link_status(),
                    || Some(grant.clone()),
                )
                .await;
        });
        let (states, input) = mpsc::channel(8);
        let server = echo_screen_server::serve_with_settings(
            ([127, 0, 0, 1], 0).into(),
            Secret::new(SETTINGS_TOKEN.into()),
            input,
            Some(send),
        )
        .await
        .unwrap();
        let actions = format!("http://{}/settings/v2?token={SETTINGS_TOKEN}", server.bound_addr);
        Self {
            server,
            actions,
            client: reqwest::Client::new(),
            stop,
            owner,
            _states: states,
        }
    }

    async fn save(&self, enabled: bool, name: &str) {
        let response = self
            .client
            .patch(&self.actions)
            .json(&json!({"version":2,"settings":{"portos":{"endpoint_enabled":enabled,"endpoint_name":name}}}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"version":2,"restart_pending":false})
        );
    }

    async fn snapshot(&self) -> Value {
        let response = self.client.get(&self.actions).send().await.unwrap();
        assert_eq!(response.status(), 200);
        response.json().await.unwrap()
    }

    async fn close(mut self) {
        self.stop.send(()).unwrap();
        self.owner.await.unwrap();
        self.server.shutdown().await.unwrap();
    }
}

fn settings_owner(home: &Path, mock: &Mock) -> SettingsHost {
    let mut config = BridgeConfig::from_toml_str(include_str!("../../../../examples/bridge.echo.toml")).unwrap();
    config.home = Some(HomeConfig {
        dir: home.join("settings"),
        agent_tools: Default::default(),
    });
    config.mcp.scopes = vec!["endpoint:read".into()];
    config.mcp.server = vec![McpServer {
        name: "portos".into(),
        url: format!("{}/mcp", mock.base),
        auth: McpAuth::PortosDeviceCode,
        bearer_file: None,
        allow_tools: vec![],
    }];
    SettingsHost::open(
        config,
        SettingsOverlay::default(),
        PromptTextsV1 {
            voice: echo_config::LIVE_INSTRUCTIONS.into(),
            runner: echo_config::RUNNER_INSTRUCTIONS.into(),
        },
        vec![],
    )
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_pair_settings_to_gateway_and_report() {
    bridge_mcp::bridge_component::drive(async {
        let home = tempfile::tempdir().unwrap();
        let mock = Mock::bind().await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        let grant = registered(&mock, home.path()).await;
        let mut gateway = Gateway::open().await;
        let settings = Settings::open(home.path(), &mock, &gateway, grant.grant.clone()).await;
        let before = settings.snapshot().await;
        assert_eq!(before["portos"]["endpoint_enabled"], false);
        assert_eq!(before["portos"]["link_status"], "linked");
        assert!(gateway.received.try_recv().is_err());
        settings.save(true, "Hall Echo").await;
        let approval = wait_approval(&settings).await;
        assert_eq!(approval["portos"]["challenge"]["user_code"], "TEST-CODE");
        assert_eq!(approval["portos"]["link_status"], "linked");
        advance(Duration::from_secs(5)).await;
        let first = GatewayEvent::Upgrade(
            "/devices/loopback%20link%2F%25".into(),
            "Bearer endpoint-access-2".into(),
            false,
        );
        assert_eq!(gateway.received.recv().await, Some(first));
        assert_installed(home.path(), &mock).await;
        settings.save(false, "Hall Echo").await;
        assert_eq!(gateway.received.recv().await, Some(GatewayEvent::Disconnected));
        advance(Duration::from_secs(61)).await;
        assert!(gateway.received.try_recv().is_err());
        assert_eq!(settings.snapshot().await["portos"]["endpoint_enabled"], false);
        settings.close().await;
        let mut grant = grant;
        grant.close().await;
    })
    .await;
}

async fn wait_approval(settings: &Settings) -> Value {
    for _ in 0..50 {
        let snapshot = settings.snapshot().await;
        if snapshot["portos"]["challenge"].is_object() {
            return snapshot;
        }
    }
    panic!("paired endpoint approval never appeared");
}

async fn assert_installed(home: &Path, mock: &Mock) {
    let bootstrapped = EndpointRegistration::open(home).unwrap();
    let install = bootstrapped.installation().await.unwrap();
    assert_eq!(install.auth_link_id, LINK);
    assert_eq!(install.route_id.as_deref(), Some(ROUTE));
    assert_eq!(install.device_name, "Hall Echo");
    let stored: Value =
        serde_json::from_slice(&fs::read(home.join("portos-endpoint/registration.json")).unwrap()).unwrap();
    assert_ne!(stored["deviceId"], ROUTE);
    let report = loop {
        if let Some(report) = mock
            .state
            .lock()
            .unwrap()
            .requests
            .iter()
            .find(|r| r.path == "/messages")
            .cloned()
        {
            break report;
        }
        tokio::task::yield_now().await;
    };
    assert_eq!(report.headers["authorization"], "Bearer endpoint-access-2");
    assert!(!report.headers.contains_key("dpop"));
    assert_eq!(
        report.json.unwrap(),
        json!({
            "header":{"namespace":echo_contracts::endpoint::ECHO_PLUGIN,"name":"report-state","version":"1.0"},
            "target":{"type":"ROUTE","value":ROUTE},
            "payload":{"state":"idle","sessionId":null,"origin":null,"volume":null,"muted":null}
        })
    );
    let state = mock.state.lock().unwrap();
    let exchange = state
        .requests
        .iter()
        .find(|r| r.path == "/plugin-auth-code-exchanges")
        .unwrap();
    assert_eq!(exchange.headers["authorization"], "Bearer endpoint-access-1");
    assert!(!exchange.headers.contains_key("dpop"));
    assert_eq!(
        exchange.json.as_ref().unwrap(),
        &json!({
            "principalId":"~self","pluginId":echo_contracts::endpoint::ECHO_PLUGIN,
            "pluginAuthenticationData":{"deviceId":stored["deviceId"],"deviceName":"Hall Echo","model":"Echo Show"}
        })
    );
}
