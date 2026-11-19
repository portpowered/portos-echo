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
        let mut url = reqwest::Url::parse("http://127.0.0.1").unwrap();
        url.set_port(Some(self.server.bound_addr.port())).unwrap();
        url.set_path(route);
        url.set_query(Some(&format!("token={TOKEN}")));
        url.into()
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

    async fn remount(&mut self, host: Box<SettingsHost>) -> SettingsService {
        self.server.shutdown().await.unwrap();
        let (owner, send) = SettingsService::resume(host);
        let (states, input) = mpsc::channel(32);
        self.server = serve_with_settings(
            "127.0.0.1:0".parse().unwrap(),
            Secret::new(TOKEN.into()),
            input,
            Some(send),
        )
        .await
        .unwrap();
        self._states = states;
        owner
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
    assert_eq!(result.0, 17);
    assert!(result.1.restart_requested);
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
    assert_eq!(result.0, Ok(()));
    assert!(!result.1.restart_requested);
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
    finish_owner(owner, cancel).await;
    assert!(send.is_closed());
    let response = sending.await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.bytes().await.unwrap().len(), 0);
    assert_eq!(fixture.overlay(), "");
    fixture.server.shutdown().await.unwrap();
}

async fn finish_owner(owner: SettingsService, cancel: bool) {
    if cancel {
        drop(owner.scope(std::future::pending::<()>(), || LinkStatusV1::Linked));
    } else {
        let (value, completion) = owner.scope(std::future::ready(23), || LinkStatusV1::Linked).await;
        assert_eq!(value, 23);
        assert!(!completion.restart_requested);
    }
}

// T3: a retained host crosses cleanup into a new HTTP/service generation.
// Replacement failure is represented by returning ownership, not an OS exec.
#[tokio::test]
async fn t3_settings_service_retains_pending_state_without_automatic_retry() {
    let (mut fixture, owner, _) = Fixture::open("[voice]\nsession_listen_seconds=9\n").await;
    let edits = async {
        fixture.patch(json!({"agent":{"max_tool_calls":4}})).await;
    };
    let (((), completion), ()) = tokio::join!(
        owner.scope(super::super::settings_restart::requested(), || LinkStatusV1::Linked),
        edits
    );
    assert!(completion.restart_requested);
    let owner = fixture.remount(completion.host).await;
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let inspect = async {
        let snapshot = fixture.snapshot().await;
        assert!(snapshot.restart_pending);
        assert_eq!(snapshot.settings.voice.session_listen_seconds, 9);
        assert_eq!(snapshot.settings.agent.max_tool_calls, 4);
        assert_eq!(snapshot.prompts.voice, echo_config::LIVE_INSTRUCTIONS);
        stop.send(()).unwrap();
    };
    let ((result, completion), ()) = tokio::join!(owner.scope(stopped, || LinkStatusV1::Linked), inspect);
    assert_eq!(result, Ok(()));
    assert!(!completion.restart_requested, "remount/snapshot must not retry exec");
    recovery_save(&mut fixture, completion.host).await;
    fixture.server.shutdown().await.unwrap();
}

async fn recovery_save(fixture: &mut Fixture, host: Box<SettingsHost>) {
    let owner = fixture.remount(host).await;
    let edits = async {
        let response = fixture
            .client
            .post(fixture.url("/settings/v1/prompts/voice"))
            .json(&json!({"version":1,"text":"retained voice\n"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"version":1,"restart_pending":false})
        );
        assert_eq!(fixture.snapshot().await.prompts.voice, "retained voice\n");
        fixture.patch(json!({"agent":{"daily_request_cap":"71"}})).await;
    };
    let (((), completion), ()) = tokio::join!(
        owner.scope(super::super::settings_restart::requested(), || LinkStatusV1::Linked),
        edits
    );
    assert!(completion.restart_requested);
    assert_eq!(
        fixture.overlay(),
        "[agent]\ndaily_request_cap = 71\nmax_tool_calls = 4\n\n[voice]\nsession_listen_seconds = 9\n"
    );
    let owner = fixture.remount(completion.host).await;
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let inspect = async {
        assert_eq!(fixture.snapshot().await.prompts.voice, "retained voice\n");
        stop.send(()).unwrap();
    };
    let ((result, completion), ()) = tokio::join!(owner.scope(stopped, || LinkStatusV1::Linked), inspect);
    assert_eq!(result, Ok(()));
    assert!(!completion.restart_requested);
}
