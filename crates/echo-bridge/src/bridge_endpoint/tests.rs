//! T3: Settings service, registered grants, REST/uplink and gateway over loopback.
use super::*;
use crate::{
    bridge_mcp,
    bridge_settings::{SettingsHost, SettingsService},
    bridge_trace::Trace,
};
use echo_config::{AgentTools, HomeConfig, McpAuth, McpServer, SettingsOverlay};
use echo_contracts::secret::Secret;
use echo_mcp_auth::{AuthOptions, LinkClient, mock::Mock};
use echo_screen_contract::settings_v1::PromptTextsV1;
use gateway_test_peer::Gateway;
use serde_json::{Value, json};
use std::{fs, path::Path};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, advance},
};

#[path = "command_tests.rs"]
mod command_tests;
#[path = "gateway_test_peer.rs"]
mod gateway_test_peer;
#[path = "setup_tests.rs"]
mod setup_tests;
#[path = "voice_tests.rs"]
mod voice_tests;

const SETTINGS_TOKEN: &str = "synthetic-paired-settings";
const LINK: &str = "loopback link/%";
const ROUTE: &str = "resolved-fake-route";
const EPOCH: u64 = 1_800_000_000;

struct Resolver;

impl RouteResolver for Resolver {
    fn resolve<'a>(&'a self, auth_link_id: &'a str, tokens: &'a dyn EndpointTokenSource) -> Resolve<'a> {
        Box::pin(async move {
            assert_eq!(auth_link_id, LINK);
            assert_eq!(tokens.access_token().await.unwrap().expose(), "endpoint-access-2");
            Some(ROUTE.into())
        })
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
    let owner = grant_owner(mock, home, EPOCH).await;
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

async fn grant_owner(mock: &Mock, home: &Path, epoch: u64) -> bridge_mcp::Runtime {
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
    bridge_mcp::Runtime::open(settings, presentation, trace, Some(epoch))
        .await
        .unwrap()
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
        Self::with_resolver(home, mock, gateway, grant, Arc::new(Resolver), false).await
    }

    async fn with_resolver(
        home: &Path,
        mock: &Mock,
        gateway: &Gateway,
        grant: Arc<Grant>,
        resolver: Arc<dyn RouteResolver>,
        base_enabled: bool,
    ) -> Self {
        let (service, send) = Self::configured(home, mock, gateway, resolver, base_enabled);
        Self::serve(service, send, grant).await
    }

    fn configured(
        home: &Path,
        mock: &Mock,
        gateway: &Gateway,
        resolver: Arc<dyn RouteResolver>,
        base_enabled: bool,
    ) -> (SettingsService, mpsc::Sender<echo_screen_server::SettingsRequest>) {
        let (owner, desired) = settings_owner(home, mock, base_enabled);
        let (service, send) = SettingsService::resume(Box::new(owner));
        let runtime = Runtime::new(
            home.into(),
            mock.base.clone(),
            resolver,
            EndpointExchange::loopback(&mock.base, REQUEST_TIMEOUT).unwrap(),
            Transport::new(&gateway.template, &mock.base, true).unwrap(),
            &desired,
        );
        (service.with_endpoint(runtime), send)
    }

    async fn serve(
        service: SettingsService,
        send: mpsc::Sender<echo_screen_server::SettingsRequest>,
        grant: Arc<Grant>,
    ) -> Self {
        Self::serve_with_owner(send, |done| {
            tokio::spawn(async move {
                let work = async {
                    let _ = done.await;
                };
                service
                    .scope_with_grant(work, || grant.link_status(), || Some(grant.clone()))
                    .await;
            })
        })
        .await
    }

    async fn serve_with_owner(
        send: mpsc::Sender<echo_screen_server::SettingsRequest>,
        start: impl FnOnce(oneshot::Receiver<()>) -> JoinHandle<()>,
    ) -> Self {
        let (stop, done) = oneshot::channel();
        let owner = start(done);
        let (states, input) = mpsc::channel(8);
        let server = echo_screen_server::serve_with_settings(
            ([127, 0, 0, 1], 0).into(),
            Secret::new(SETTINGS_TOKEN.into()),
            input,
            Some(send),
        )
        .await
        .unwrap();
        let actions = format!(
            "http://127.0.0.1:{}/settings/v2?token={SETTINGS_TOKEN}", // lint-rules: allow-literal port interpolated from the loopback Settings listener
            server.bound_addr.port()
        );
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

    async fn unlink(&self) {
        let response = self
            .client
            .post(self.actions.replace("/settings/v2?", "/settings/v2/portos?"))
            .json(&json!({"version":2, "operation":"unlink", "confirm":true}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 202);
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"version":2, "operation":"unlink"})
        );
    }

    async fn close(mut self) {
        self.stop.send(()).unwrap();
        self.owner.await.unwrap();
        self.server.shutdown().await.unwrap();
    }
}

fn settings_owner(home: &Path, mock: &Mock, base_enabled: bool) -> (SettingsHost, echo_config::PortosEndpoint) {
    let mut config = BridgeConfig::from_toml_str(include_str!("../../../../examples/bridge.echo.toml")).unwrap();
    config.home = Some(HomeConfig {
        dir: home.join("settings"),
        agent_tools: AgentTools::default(),
    });
    config.mcp.scopes = vec!["endpoint:read".into()];
    config.portos_endpoint.api_base = Some(mock.base.replacen("http://", "https://", 1));
    config.portos_endpoint.gateway_url = Some("wss://127.0.0.1:1/devices/{authLinkId}".into());
    if base_enabled {
        config.portos_endpoint.enabled = true;
        config.portos_endpoint.device_name = "Hall Echo".into();
    }
    config.mcp.server = vec![McpServer {
        name: "portos".into(),
        url: format!("{}/mcp", mock.base),
        auth: McpAuth::PortosDeviceCode,
        bearer_file: None,
        allow_tools: vec![],
    }];
    let text = fs::read_to_string(home.join("settings/settings.toml"));
    let overlay = if text
        .as_ref()
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        SettingsOverlay::default()
    } else {
        SettingsOverlay::from_toml_str(&text.unwrap()).unwrap()
    };
    let desired = overlay.apply_to(&config).unwrap().portos_endpoint;
    let owner = SettingsHost::open(
        config,
        overlay,
        PromptTextsV1 {
            voice: echo_config::LIVE_INSTRUCTIONS.into(),
            runner: echo_config::RUNNER_INSTRUCTIONS.into(),
        },
        vec![],
    )
    .unwrap();
    (owner, desired)
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_pair_settings_to_gateway_and_report() {
    Box::pin(bridge_mcp::bridge_component::drive(async {
        let home = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        let mut grant = registered(&mock, home.path()).await;
        let mut gateway = Gateway::open().await;
        let settings = Settings::open(home.path(), &mock, &gateway, grant.grant.clone()).await;
        let before = settings.snapshot().await;
        assert_eq!(before["portos"]["endpoint_enabled"], false);
        assert_eq!(before["portos"]["link_status"], "linked");
        assert!(gateway.received.try_recv().is_err());
        settings.save(true, "Hall Echo").await;
        gateway.assert_upgrade(4).await;
        assert_eq!(settings.snapshot().await["portos"]["challenge"], Value::Null);
        assert_installed(home.path(), &mock).await;
        let settings = restart_and_rename(settings, home.path(), &mock, &mut gateway, &mut grant, started).await;
        setup_tests::assert_rest_audiences(&mock).await;
        let device_id = read_install(home.path())["deviceId"].clone();
        settings.unlink().await;
        gateway.assert_disconnect().await;
        assert_unlinked(home.path(), &settings, device_id).await;
        setup_tests::assert_wrong_gateway_audience(&gateway).await;
        settings.close().await;
        grant.close().await;
    }))
    .await;
}

async fn restart_and_rename(
    settings: Settings,
    home: &Path,
    mock: &Mock,
    gateway: &mut Gateway,
    grant: &mut bridge_mcp::Runtime,
    started: Instant,
) -> Settings {
    disable_and_resume(&settings, gateway).await;
    settings.save(true, "Kitchen").await;
    gateway.assert_disconnect().await;
    gateway.assert_upgrade(4).await;
    settings.close().await;
    gateway.assert_disconnect().await;
    grant.close().await;
    // Reopen at the simulated wall time of the still-running issuer.
    *grant = grant_owner(mock, home, EPOCH + started.elapsed().as_secs()).await;

    let reopened = Settings::open(home, mock, gateway, grant.grant.clone()).await;
    gateway.assert_upgrade(6).await;
    assert_eq!(reopened.snapshot().await["portos"]["endpoint_name"], "Kitchen");
    assert_rename(home, mock);
    reopened
}

async fn disable_and_resume(settings: &Settings, gateway: &mut Gateway) {
    settings.save(false, "Hall Echo").await;
    gateway.assert_disconnect().await;
    advance(Duration::from_secs(61)).await;
    assert!(gateway.received.try_recv().is_err());
    assert_eq!(settings.snapshot().await["portos"]["endpoint_enabled"], false);
    settings.save(true, "Hall Echo").await;
    gateway.assert_upgrade(4).await;
}

fn assert_rename(home: &Path, mock: &Mock) {
    let stored = read_install(home);
    let state = mock.state.lock().unwrap();
    let exchanges: Vec<_> = state
        .requests
        .iter()
        .filter(|request| request.path == "/plugin-auth-code-exchanges")
        .collect();
    assert_eq!(exchanges.len(), 2);
    let rename = exchanges[1].json.as_ref().unwrap();
    assert_eq!(rename["authLinkId"], LINK);
    assert_eq!(rename["pluginAuthenticationData"]["deviceId"], stored["deviceId"]);
    assert_eq!(rename["pluginAuthenticationData"]["deviceName"], "Kitchen");
    assert_ne!(
        exchanges[0].headers["idempotency-key"],
        exchanges[1].headers["idempotency-key"]
    );
    assert_eq!(
        state
            .requests
            .iter()
            .filter(|request| request.path == "/custom/link")
            .count(),
        1
    );
}

async fn assert_unlinked(home: &Path, settings: &Settings, device_id: Value) {
    let state = read_install(home);
    assert_eq!(state, json!({"version":1,"deviceId":device_id}));
    for _ in 0..50 {
        if settings.snapshot().await["portos"]["link_status"] == "link_required" {
            return;
        }
    }
    assert_eq!(settings.snapshot().await["portos"]["link_status"], "link_required");
}

fn read_install(home: &Path) -> Value {
    serde_json::from_slice(&fs::read(home.join("portos-endpoint/registration.json")).unwrap()).unwrap()
}

async fn assert_installed(home: &Path, mock: &Mock) {
    let bootstrapped = EndpointRegistration::open(home).unwrap();
    let install = bootstrapped.installation().await.unwrap();
    assert_eq!(install.auth_link_id, LINK);
    assert_eq!(install.route_id.as_deref(), Some(ROUTE));
    assert_eq!(install.device_name, "Hall Echo");
    let stored = read_install(home);
    assert_ne!(stored["deviceId"], ROUTE);
    assert_initial_report(mock).await;
    assert_install_exchange(mock, &stored);
}

async fn assert_initial_report(mock: &Mock) {
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
    assert_eq!(report.headers["authorization"], "Bearer endpoint-access-4");
    assert!(!report.headers.contains_key("dpop"));
    assert_eq!(
        report.json.unwrap(),
        json!({
            "header":{"namespace":echo_contracts::endpoint::ECHO_PLUGIN,"name":"report-state","version":"1.0"},
            "target":{"type":"ROUTE","value":ROUTE},
            "payload":{"state":"idle","sessionId":null,"origin":null,"volume":null,"muted":null}
        })
    );
}

fn assert_install_exchange(mock: &Mock, stored: &Value) {
    let state = mock.state.lock().unwrap();
    assert!(state.endpoint.mcp_claimed);
    assert!(state.endpoint.endpoint_claimed);
    let exchange = state
        .requests
        .iter()
        .find(|r| r.path == "/plugin-auth-code-exchanges")
        .unwrap();
    assert_eq!(exchange.headers["authorization"], "Bearer endpoint-access-2");
    assert!(!exchange.headers.contains_key("dpop"));
    assert_eq!(
        exchange.json.as_ref().unwrap(),
        &json!({
            "principalId":"~self","pluginId":echo_contracts::endpoint::ECHO_PLUGIN,
            "pluginAuthenticationData":{"deviceId":stored["deviceId"],"deviceName":"Hall Echo","model":"Echo Show"}
        })
    );
}
