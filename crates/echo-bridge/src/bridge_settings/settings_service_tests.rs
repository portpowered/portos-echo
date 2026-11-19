//! T3: real loopback HTTP and file store, controlled service/cleanup boundaries.
//! No configured binary or exec claim; each witness owns its home and listener.
use super::*;
use echo_config::BridgeConfig;
use echo_contracts::secret::Secret;
use echo_screen_contract::settings_v1::SettingsSnapshotV1;
use echo_screen_server::{ScreenServer, serve_with_settings};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use std::{cell::Cell, fs, time::Duration};

const TOKEN: &str = "synthetic-service-pair";

struct Fixture {
    root: tempfile::TempDir,
    server: ScreenServer,
    _states: mpsc::Sender<echo_screen_contract::UiState>,
    client: Client,
}

impl Fixture {
    async fn open(overlay: &str) -> (Self, SettingsService, mpsc::Sender<SettingsRequest>) {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir(&home).unwrap();
        let text = format!(
            "[echo]\nhost='127.0.0.1'\n[live]\nvoice='alloy'\ncredential_file='absent/live'\n[agent]\n[mcp]\n[screen]\npairing_token_file='absent/pairing'\n[audit]\nlog_dir='absent/logs'\n[home]\ndir='{}'\n",
            home.display()
        );
        let base_path = root.path().join("bridge.toml");
        fs::write(&base_path, text).unwrap();
        fs::write(home.join("settings.toml"), overlay).unwrap();
        let config = BridgeConfig::load_effective(&base_path).unwrap();
        assert_eq!(config.base.agent.max_tool_calls, 8);
        let (owner, send) = SettingsService::open(
            config,
            PromptTextsV1 {
                voice: echo_config::LIVE_INSTRUCTIONS.into(),
                runner: echo_config::RUNNER_INSTRUCTIONS.into(),
            },
        )
        .unwrap();
        let (states, input) = mpsc::channel(32);
        let server = serve_with_settings(
            "127.0.0.1:0".parse().unwrap(),
            Secret::new(TOKEN.into()),
            input,
            Some(send.clone()),
        )
        .await
        .unwrap();
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        (
            Self {
                root,
                server,
                _states: states,
                client,
            },
            owner,
            send,
        )
    }

    fn url(&self, route: &str) -> String {
        format!("http://{}{route}?token={TOKEN}", self.server.bound_addr)
    }

    async fn snapshot(&self) -> SettingsSnapshotV1 {
        let response = self.client.get(self.url("/settings/v1")).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.json().await.unwrap()
    }

    async fn patch(&self, settings: Value) -> Value {
        let response = self
            .client
            .post(self.url("/settings/v1"))
            .json(&json!({"version":1,"settings":settings}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.json().await.unwrap()
    }

    fn overlay(&self) -> String {
        fs::read_to_string(self.root.path().join("home/settings.toml")).unwrap()
    }
}

#[tokio::test]
async fn t3_settings_service_signals_after_durable_save_and_serves_during_cleanup() {
    let (mut fixture, owner, send) = Fixture::open("[agent]\nmax_tool_calls=4\n").await;
    let base_path = fixture.root.path().join("bridge.toml");
    let base = fs::read(&base_path).unwrap();
    let (release, cleanup) = tokio::sync::oneshot::channel();
    let (entered, boundary) = tokio::sync::oneshot::channel();
    let work = async {
        super::super::settings_restart::requested().await;
        entered.send(()).unwrap();
        cleanup.await.unwrap();
        17
    };
    let edits = async {
        assert_eq!(fixture.snapshot().await.settings.agent.max_tool_calls, 4);
        assert_eq!(
            fixture.patch(json!({"agent":{"daily_request_cap":"71"}})).await,
            json!({"version":1,"restart_pending":true})
        );
        boundary.await.unwrap();
        assert_eq!(
            fixture.patch(json!({"codex":{"voice":"marin"}})).await,
            json!({"version":1,"restart_pending":true})
        );
        let snapshot = fixture.snapshot().await;
        assert_eq!(snapshot.settings.agent.daily_request_cap.as_str(), "71");
        assert_eq!(snapshot.settings.codex.voice, "marin");
        assert_eq!(snapshot.settings.agent.max_tool_calls, 4);
        release.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(owner.scope(work, || LinkStatusV1::Linked), edits);
    assert_eq!(result, (17, true));
    assert!(send.is_closed());
    assert_eq!(
        fixture.overlay(),
        "[agent]\ndaily_request_cap = 71\nmax_tool_calls = 4\n\n[codex]\nvoice = \"marin\"\n"
    );
    assert_eq!(fs::read(base_path).unwrap(), base);
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn t3_settings_service_samples_link_and_prompt_noop_validation_never_restart() {
    let (mut fixture, owner, _) = Fixture::open("invalid = 'ignored'").await;
    let linked = Cell::new(LinkStatusV1::LinkRequired);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let edits = async {
        let snapshot = fixture.snapshot().await;
        assert_eq!(snapshot.link_status, LinkStatusV1::LinkRequired);
        assert_eq!(
            snapshot.warnings,
            ["settings.toml: ignored overlay: config: unknown field"]
        );
        linked.set(LinkStatusV1::Linked);
        assert_eq!(fixture.snapshot().await.link_status, LinkStatusV1::Linked);
        let response = fixture
            .client
            .post(fixture.url("/settings/v1/prompts/voice"))
            .json(&json!({"version":1,"text":"New voice instructions"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"version":1,"restart_pending":false})
        );
        assert_eq!(fixture.snapshot().await.prompts.voice, "New voice instructions");
        assert_eq!(
            fixture.patch(json!({"agent":{"max_tool_calls":8}})).await,
            json!({"version":1,"restart_pending":false})
        );
        let response = fixture
            .client
            .post(fixture.url("/settings/v1"))
            .json(&json!({"version":1,"settings":{"agent":{"max_tool_calls":0}}}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"version":1,"message":"agent.max_tool_calls: must be within 1..=32"})
        );
        assert_eq!(fixture.overlay(), "[agent]\nmax_tool_calls = 8\n");
        assert!(!fixture.snapshot().await.restart_pending);
        stop.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(owner.scope(stopped, || linked.get()), edits);
    assert_eq!(result, (Ok(()), false));
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn t3_settings_service_completion_refuses_queued_save_and_closes_receipt() {
    queued_save(false).await;
    queued_save(true).await;
}

async fn queued_save(cancel: bool) {
    let (mut fixture, mut owner, send) = Fixture::open("").await;
    let sending = fixture
        .client
        .post(fixture.url("/settings/v1"))
        .json(&json!({"version":1,"settings":{"agent":{"max_tool_calls":4}}}))
        .send();
    tokio::pin!(sending);
    // Receiving proves HTTP has queued this authorized operation; put it back
    // before polling an already-complete lifecycle, without a timer or sleep.
    let request = tokio::select! {
        request = owner.inbox.recv() => request.unwrap(),
        response = &mut sending => unreachable!("responded before commit: {response:?}"),
    };
    assert!(send.send(request).await.is_ok());
    if cancel {
        drop(owner.scope(std::future::pending::<()>(), || LinkStatusV1::Linked));
    } else {
        assert_eq!(
            owner.scope(std::future::ready(23), || LinkStatusV1::Linked).await,
            (23, false)
        );
    }
    assert!(send.is_closed());
    let response = sending.await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.bytes().await.unwrap().len(), 0);
    assert_eq!(fixture.overlay(), "");
    fixture.server.shutdown().await.unwrap();
}
