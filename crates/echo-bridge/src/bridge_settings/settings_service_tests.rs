//! T3: real loopback HTTP and file store, controlled service/cleanup boundaries.
//! No configured binary or exec claim; each witness owns its home and listener.
use super::*;
use echo_config::{BridgeConfig, SettingsOverlay};
use echo_contracts::secret::Secret;
use echo_screen_contract::settings_v1::SettingsSnapshotV1;
use echo_screen_server::{ScreenServer, serve_with_settings};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use std::{cell::Cell, fs, time::Duration};

const TOKEN: &str = "synthetic-service-pair";

#[cfg(unix)]
#[path = "settings_service_tests/muse.rs"]
mod muse;

struct Fixture {
    root: tempfile::TempDir,
    server: ScreenServer,
    states: mpsc::Sender<echo_screen_contract::UiState>,
    client: Client,
}

impl Fixture {
    async fn open(overlay: &str) -> (Self, SettingsService, mpsc::Sender<SettingsRequest>) {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir(&home).unwrap();
        let text = format!(
            "[echo]\nhost='127.0.0.1'\n[live]\nvoice='cove'\ncredential_file='absent/live'\n[agent]\n[mcp]\n[screen]\npairing_token_file='absent/pairing'\n[audit]\nlog_dir='absent/logs'\n[home]\ndir='{}'\n",
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
                states,
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
        self.states = states;
        assert!(!self.states.is_closed());
        owner
    }
}

#[tokio::test]
async fn t3_settings_service_signals_after_durable_save_and_serves_during_cleanup() {
    let (mut fixture, owner, send) = Fixture::open("[agent]\nmax_tool_calls=4\n").await;
    let trace_path = fixture.root.path().join("trace");
    let owner = owner.with_trace(crate::bridge_trace::Trace::open(&trace_path, "chatgpt").unwrap());
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
            fixture.patch(json!({"codex":{"voice":"maple"}})).await,
            json!({"version":1,"restart_pending":true})
        );
        let snapshot = fixture.snapshot().await;
        assert_eq!(snapshot.settings.agent.daily_request_cap.as_str(), "71");
        assert_eq!(snapshot.settings.codex.voice, "maple");
        assert_eq!(snapshot.settings.agent.max_tool_calls, 4);
        release.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(owner.scope(work, || LinkStatusV1::Linked), edits);
    assert_eq!(result.0, 17);
    assert_eq!(
        operational_events(&trace_path),
        [
            json!({"event":"settings.saved","restart_pending":true}),
            json!({"event":"settings.reexec_pending"}),
            json!({"event":"settings.saved","restart_pending":true}),
        ]
    );
    assert!(result.1.restart_requested);
    assert!(send.is_closed());
    assert_eq!(
        fixture.overlay(),
        "[agent]\ndaily_request_cap = 71\nmax_tool_calls = 4\n\n[codex]\nvoice = \"maple\"\n"
    );
    assert_eq!(fs::read(base_path).unwrap(), base);
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn t3_settings_service_samples_link_and_prompt_noop_validation_never_restart() {
    let (mut fixture, owner, _) = Fixture::open("invalid = 'ignored'").await;
    let trace_path = fixture.root.path().join("trace");
    let owner = owner.with_trace(crate::bridge_trace::Trace::open(&trace_path, "chatgpt").unwrap());
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
    assert_eq!(
        operational_events(&trace_path),
        [
            json!({"event":"prompt.saved","prompt":"voice"}),
            json!({"event":"settings.saved","restart_pending":false}),
        ]
    );
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
    refused_save(sending.await).await;
    assert_eq!(fixture.overlay(), "");
    fixture.server.shutdown().await.unwrap();
}

async fn refused_save(response: reqwest::Result<reqwest::Response>) {
    let response = response.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.bytes().await.unwrap().len(), 0);
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
        fixture.patch(json!({"agent":{"daily_request_cap":"4"}})).await;
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
        assert_eq!(snapshot.settings.agent.daily_request_cap.as_str(), "4");
        assert_eq!(snapshot.prompts.voice, echo_config::LIVE_INSTRUCTIONS);
        let before = fixture.overlay();
        let reply = fixture
            .client
            .patch(fixture.url("/settings/v2"))
            .json(&json!({"version":2,"settings":{"agent":{"max_tool_calls":3,"task_deadline_seconds":5}}}))
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status(), StatusCode::OK);
        assert_eq!(
            reply.json::<Value>().await.unwrap(),
            json!({"version":2,"restart_pending":true})
        );
        assert_eq!(fixture.overlay(), before);
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
        "[agent]\ndaily_request_cap = 71\n\n[voice]\nsession_listen_seconds = 9\n"
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

fn operational_events(path: &std::path::Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| {
            let mut row: Value = serde_json::from_str(line).unwrap();
            let object = row.as_object_mut().unwrap();
            assert_eq!(object.remove("provider_auth_mode"), Some(json!("chatgpt")));
            assert!(object.remove("elapsed_ms").unwrap().is_u64());
            row
        })
        .collect()
}

#[tokio::test]
async fn t3_screen_pack_service_selection_never_requests_or_retries_voice_restart() {
    let (mut fixture, owner, _) = Fixture::open("").await;
    let home = fixture.root.path().join("home");
    let pack = home.join("screens/local");
    fs::create_dir_all(&pack).unwrap();
    fs::write(
        pack.join("pack.json"),
        r#"{"format":1,"name":"local","version":"1.0.0","character":{"rest":"rest.png"}}"#,
    )
    .unwrap();
    fs::write(pack.join("rest.png"), "generated-rest").unwrap();
    let _jobs = fixture.server.install_configured_packs(home.clone(), None, vec![]);
    let (finish, finished) = tokio::sync::oneshot::channel();
    let edits = async {
        assert_eq!(
            pack_selection(&fixture, "local").await,
            json!({"version":1,"restart_pending":false})
        );
        finish.send(()).unwrap();
    };
    let ((result, completion), ()) = tokio::join!(owner.scope(finished, || LinkStatusV1::Linked), edits);
    assert_eq!(result, Ok(()));
    assert!(!completion.restart_requested);
    let owner = fixture.remount(completion.host).await;
    let edits = async {
        fixture.patch(json!({"agent":{"daily_request_cap":"4"}})).await;
    };
    let (((), completion), ()) = tokio::join!(
        owner.scope(super::super::settings_restart::requested(), || LinkStatusV1::Linked),
        edits
    );
    assert!(completion.restart_requested);
    let owner = fixture.remount(completion.host).await;
    let _jobs = fixture.server.install_configured_packs(home, None, vec![]);
    let (finish, finished) = tokio::sync::oneshot::channel();
    let edits = async {
        assert_eq!(
            pack_selection(&fixture, "").await,
            json!({"version":1,"restart_pending":true})
        );
        assert!(fixture.snapshot().await.restart_pending);
        assert_eq!(fixture.snapshot().await.settings.agent.daily_request_cap.as_str(), "4");
        let overlay = SettingsOverlay::from_toml_str(&fixture.overlay()).unwrap();
        assert_eq!(overlay.screen_pack(), Some(""));
        finish.send(()).unwrap();
    };
    let ((result, completion), ()) = tokio::join!(owner.scope(finished, || LinkStatusV1::Linked), edits);
    assert_eq!(result, Ok(()));
    assert!(
        !completion.restart_requested,
        "pack-only save must not retry an old voice restart"
    );
    fixture.server.shutdown().await.unwrap();
}

async fn pack_selection(fixture: &Fixture, selection: &str) -> Value {
    let response = fixture
        .client
        .post(fixture.url("/screen-packs/v1/selection"))
        .json(&json!({"version":1,"screen_pack":selection}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

#[tokio::test]
async fn t3_screen_pack_external_settings_adoption_keeps_runtime_and_exec_unchanged() {
    let (mut fixture, owner, _) = Fixture::open("").await;
    let home = fixture.root.path().join("home");
    let base = fs::read(fixture.root.path().join("bridge.toml")).unwrap();
    let _jobs = fixture.server.install_configured_packs(home.clone(), None, vec![]);
    let (finish, finished) = tokio::sync::oneshot::channel();
    let edits = async {
        fs::write(
            home.join("settings.toml"),
            "screen_pack=''\n[agent]\nmax_tool_calls=4\n",
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = fixture.snapshot().await;
                assert!(!snapshot.restart_pending);
                if snapshot.settings.agent.max_tool_calls == 4 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(fixture.overlay(), "screen_pack=''\n[agent]\nmax_tool_calls=4\n");
        finish.send(()).unwrap();
    };
    let ((result, completion), ()) = tokio::join!(owner.scope(finished, || LinkStatusV1::Linked), edits);
    assert_eq!(result, Ok(()));
    assert!(!completion.restart_requested);
    assert_eq!(fs::read(fixture.root.path().join("bridge.toml")).unwrap(), base);
    let mut host = completion.host;
    let snapshot = match host.transaction(SettingsOperation::Snapshot, LinkStatusV1::Linked) {
        SettingsReply::Snapshot(snapshot) => Some(snapshot),
        _ => None,
    };
    let snapshot = snapshot.unwrap();
    assert_eq!(snapshot.settings.agent.max_tool_calls, 4);
    assert!(!snapshot.restart_pending);
    assert_eq!(
        SettingsOverlay::from_toml_str(&fixture.overlay())
            .unwrap()
            .screen_pack(),
        Some("")
    );
    fixture.server.shutdown().await.unwrap();
}
