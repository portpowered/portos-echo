//! Component T3: real authorized HTTP -> bridge host -> config and atomic files.
//! No product startup/Live call/exec claim; each case owns loopback and a temp home.
use super::*;
use echo_contracts::secret::Secret;
use echo_screen_contract::settings_v1::{LinkStatusV1, PromptTextsV1, SettingsSnapshotV1};
use echo_screen_server::{ScreenServer, serve_with_settings, settings_channel};
use reqwest::{Client, StatusCode};
use std::io::Write as _;
use std::{fs, path::Path, time::Duration};
use tokio::{sync::mpsc, task::JoinHandle};

const TOKEN: &str = "synthetic-settings-pair";
const BASE: &str = "[echo]\nhost='127.0.0.1'\n[live]\nvoice='cove'\ncredential_file='absent/live'\n[agent]\n[mcp]\n[screen]\npairing_token_file='absent/pairing'\n[audit]\nlog_dir='absent/logs'\n";

pub(crate) struct Fixture {
    root: tempfile::TempDir,
    server: ScreenServer,
    states: mpsc::Sender<echo_screen_contract::UiState>,
    host: JoinHandle<()>,
    client: Client,
}

impl Fixture {
    pub(crate) async fn open() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("bridge.toml"), BASE).unwrap();
        let mut base = BridgeConfig::from_toml_str(BASE).unwrap();
        base.home = Some(echo_config::HomeConfig {
            dir: root.path().join("home"),
            agent_tools: echo_config::AgentTools::default(),
        });
        let mut owner = SettingsHost::open(
            base,
            SettingsOverlay::default(),
            PromptTextsV1 {
                voice: echo_config::LIVE_INSTRUCTIONS.into(),
                runner: echo_config::RUNNER_INSTRUCTIONS.into(),
            },
            vec![],
        )
        .unwrap();
        let (send, mut inbox) = settings_channel();
        let (states, input) = mpsc::channel(32);
        let server = serve_with_settings(
            "127.0.0.1:0".parse().unwrap(),
            Secret::new(TOKEN.into()),
            input,
            Some(send),
        )
        .await
        .unwrap();
        let host = tokio::spawn(async move {
            while let Some(request) = inbox.recv().await {
                request
                    .commit(|operation| owner.transaction(operation, LinkStatusV1::Unconfigured))
                    .await;
            }
        });
        Self {
            root,
            server,
            states,
            host,
            client: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        }
    }

    pub(crate) fn url(&self, route: &str) -> String {
        let mut url = reqwest::Url::parse("http://127.0.0.1").unwrap();
        url.set_port(Some(self.server.bound_addr.port())).unwrap();
        url.set_path(route);
        url.set_query(Some(&format!("token={TOKEN}")));
        url.into()
    }

    pub(crate) fn path(&self, file: &str) -> PathBuf {
        self.root.path().join("home").join(file)
    }

    pub(crate) async fn snapshot(&self) -> SettingsSnapshotV1 {
        let response = self.client.get(self.url("/settings/v1")).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.json().await.unwrap()
    }

    async fn patch(&self, fields: serde_json::Value) -> (StatusCode, serde_json::Value) {
        let response = self
            .client
            .post(self.url("/settings/v1"))
            .json(&serde_json::json!({"version":1,"settings":fields}))
            .send()
            .await
            .unwrap();
        (response.status(), response.json().await.unwrap())
    }

    pub(crate) async fn close(mut self) {
        self.server.shutdown().await.unwrap();
        self.host.await.unwrap();
    }
}

#[tokio::test]
async fn t3_settings_host_http_sparse_merge_validation_permissions_and_base_preservation() {
    let fixture = Fixture::open().await;
    let original = fixture.snapshot().await;
    assert_eq!(original.settings.codex.voice, "cove");
    assert_eq!(original.prompts.voice, echo_config::LIVE_INSTRUCTIONS);
    assert!(!original.restart_pending);
    assert_eq!(
        fixture.patch(serde_json::json!({"agent":{"max_tool_calls":4}})).await,
        (StatusCode::OK, serde_json::json!({"version":1,"restart_pending":false}))
    );
    assert_eq!(
        fs::read_to_string(fixture.path("settings.toml")).unwrap(),
        "[agent]\nmax_tool_calls = 4\n"
    );
    invalid_save_retains_file(&fixture).await;
    assert_eq!(
        fixture.patch(serde_json::json!({"codex":{"voice":"maple"}})).await.0,
        StatusCode::OK
    );
    let snapshot = fixture.snapshot().await;
    assert_eq!(snapshot.settings.agent.max_tool_calls, 4);
    assert_eq!(snapshot.settings.codex.voice, "maple");
    assert!(snapshot.restart_pending);
    assert_eq!(snapshot.settings.voice, original.settings.voice);
    assert_eq!(snapshot.link_status, LinkStatusV1::Unconfigured);
    assert_eq!(snapshot.bridge_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        snapshot.allowed_voices,
        echo_config::realtime_voices().collect::<Vec<_>>()
    );
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("bridge.toml")).unwrap(),
        BASE
    );
    let overlay = fs::read_to_string(fixture.path("settings.toml")).unwrap();
    assert_eq!(overlay, "[agent]\nmax_tool_calls = 4\n\n[codex]\nvoice = \"maple\"\n");
    owner_only(&fixture.path("settings.toml"));
    fixture.close().await;
}

/// The pin refuses a v2-only voice for the v3 start, so a save never stores one.
#[tokio::test]
async fn t3_settings_host_http_refuses_a_voice_the_pinned_v3_start_rejects() {
    let fixture = Fixture::open().await;
    assert_eq!(
        fixture.snapshot().await.allowed_voices,
        [
            "default", "arbor", "breeze", "cove", "ember", "juniper", "maple", "sol", "spruce", "vale"
        ]
    );
    for voice in ["alloy", "marin", "cedar"] {
        assert_eq!(
            fixture.patch(serde_json::json!({"codex":{"voice":voice}})).await,
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                serde_json::json!({
                    "version":1,
                    "message":"codex.voice: not supported by the pinned realtime version; choose default or a listed voice"
                })
            )
        );
    }
    assert!(!fixture.path("settings.toml").exists());
    let snapshot = fixture.snapshot().await;
    assert_eq!(snapshot.settings.codex.voice, "cove");
    assert!(!snapshot.restart_pending);
    fixture.close().await;
}

async fn invalid_save_retains_file(fixture: &Fixture) {
    let before = fs::read(fixture.path("settings.toml")).unwrap();
    assert_eq!(
        fixture.patch(serde_json::json!({"agent":{"max_tool_calls":33}})).await,
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            serde_json::json!({"version":1,"message":"agent.max_tool_calls: must be within 1..=32"})
        )
    );
    assert_eq!(fs::read(fixture.path("settings.toml")).unwrap(), before);
}

fn owner_only(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o7777, 0o600);
}

#[tokio::test]
async fn t3_settings_host_http_guards_do_not_write_and_invalid_values_do_not_leak() {
    let fixture = Fixture::open().await;
    let mut url = reqwest::Url::parse(&fixture.url("/settings/v1")).unwrap();
    url.set_query(None);
    for (url, origin, expected) in [
        (url.to_string(), None, StatusCode::UNAUTHORIZED),
        (
            fixture.url("/settings/v1"),
            Some("http://127.0.0.1:1"),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let mut request = fixture
            .client
            .post(url)
            .json(&serde_json::json!({"version":1,"settings":{"agent":{"max_tool_calls":4}}}));
        if let Some(origin) = origin {
            request = request.header("Origin", origin);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), expected);
        assert_eq!(response.bytes().await.unwrap().len(), 0);
        assert!(!fixture.path("settings.toml").exists());
    }
    for (fields, message) in [
        (
            serde_json::json!({"codex":{"voice":"SYNTHETIC_VALUE\n\u{8a9e}"}}),
            "codex.voice: expected default or a pinned realtime voice",
        ),
        (
            serde_json::json!({"agent":{"daily_request_cap":"9223372036854775808"}}),
            "agent.daily_request_cap: must fit a TOML integer",
        ),
    ] {
        assert_eq!(
            fixture.patch(fields).await,
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                serde_json::json!({"version":1,"message":message})
            )
        );
        assert!(!fixture.path("settings.toml").exists());
        assert!(!fixture.snapshot().await.restart_pending);
    }
    fixture.close().await;
}

#[tokio::test]
async fn t3_settings_host_http_prompt_exact_bytes_cache_and_validation_without_restart() {
    let fixture = Fixture::open().await;
    let text = "\u{8a9e}\n<script>inert text</script>\n";
    for kind in ["voice", "runner"] {
        let url = fixture.url(&format!("/settings/v1/prompts/{kind}"));
        let response = fixture
            .client
            .post(&url)
            .json(&serde_json::json!({"version":1,"text":text}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap(),
            serde_json::json!({"version":1,"restart_pending":false})
        );
        let path = fixture.path(&format!("prompts/{kind}.md"));
        assert_eq!(fs::read(&path).unwrap(), text.as_bytes());
        owner_only(&path);
        let response = fixture
            .client
            .post(url)
            .json(&serde_json::json!({"version":1,"text":" \n\t"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap(),
            serde_json::json!({"version":1,"message":format!("prompts/{kind}.md: empty prompt")})
        );
        assert_eq!(fs::read(path).unwrap(), text.as_bytes());
    }
    let snapshot = fixture.snapshot().await;
    assert_eq!(
        snapshot.prompts,
        PromptTextsV1 {
            voice: text.into(),
            runner: text.into()
        }
    );
    assert!(!snapshot.restart_pending);
    assert!(!fixture.path("settings.toml").exists());
    fixture.close().await;
}

#[path = "settings_http_tests/pack_cases.rs"]
mod pack_cases;

#[tokio::test]
async fn t3_settings_v2_http_refuses_wrong_version_origin_and_invalid_config() {
    use serde_json::{Value, json};
    let fixture = Fixture::open().await;
    let url = fixture.url("/settings/v2");
    let before = snapshot_v2(&fixture, &url).await;
    assert_v2_guards(&fixture, &url).await;
    let response = fixture
        .client
        .patch(&url)
        .json(&json!({"version":2,"settings":{"agent":{"max_tool_calls":33}}}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        body,
        json!({"version":2,"message":"agent.max_tool_calls: must be within 1..=32"})
    );
    writeln!(std::io::stdout().lock(), "Refused invalid config with {body}").unwrap();
    assert!(!fixture.path("settings.toml").exists());
    assert!(!fixture.snapshot().await.restart_pending);
    assert_eq!(snapshot_v2(&fixture, &url).await, before);
    fixture.close().await;
}

async fn assert_v2_guards(fixture: &Fixture, url: &str) {
    use serde_json::json;

    let unauthenticated = url.split('?').next().unwrap();
    let fast = |version| json!({"version":version,"settings":{"codex":{"delegate_fast":true}}});
    for (request, expected) in [
        (fixture.client.get(unauthenticated), StatusCode::UNAUTHORIZED),
        (
            fixture.client.patch(unauthenticated).json(&fast(2)),
            StatusCode::UNAUTHORIZED,
        ),
        (
            fixture
                .client
                .patch(url)
                .header("Origin", "http://127.0.0.1:1")
                .json(&fast(2)),
            StatusCode::FORBIDDEN,
        ),
        (
            fixture.client.patch(url).body("invalid"),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            fixture
                .client
                .patch(url)
                .json(&json!({"version":1,"settings":{"agent":{"max_tool_calls":6}}})),
            StatusCode::BAD_REQUEST,
        ),
        (
            fixture
                .client
                .patch(url)
                .json(&json!({"version":2,"settings":{"agent":{}}})),
            StatusCode::BAD_REQUEST,
        ),
        (
            fixture
                .client
                .patch(url)
                .json(&json!({"version":2,"settings":{"codex":{"delegate_fast":null}}})),
            StatusCode::BAD_REQUEST,
        ),
        (
            fixture
                .client
                .patch(url)
                .json(&json!({"version":2,"settings":{"codex":{"delegate_fast":"false"}}})),
            StatusCode::BAD_REQUEST,
        ),
        (
            fixture.client.post(fixture.url("/settings/v1")).json(&fast(1)),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        assert_empty_refusal(request, expected).await;
    }
}

async fn assert_empty_refusal(request: reqwest::RequestBuilder, expected: StatusCode) {
    let response = request.send().await.unwrap();
    assert_eq!(response.status(), expected);
    assert_eq!(response.bytes().await.unwrap().as_ref(), b"");
    writeln!(
        std::io::stdout().lock(),
        "Refused paired boundary request with {expected}"
    )
    .unwrap();
}

#[tokio::test]
async fn t3_settings_v2_http_failed_rename_retains_effective_and_v1_shape() {
    use serde_json::{Value, json};
    let fixture = Fixture::open().await;
    let v1 = fixture.snapshot().await;
    let url = fixture.url("/settings/v2");
    let before = snapshot_v2(&fixture, &url).await;
    assert_eq!(before["settings"]["codex"]["delegate_fast"], false);
    let base = fixture.root.path().join("bridge.toml");
    let original_base = fs::read(&base).unwrap();
    let path = fixture.path("settings.toml");
    fs::create_dir(&path).unwrap();
    let fields = json!({"version":2,"settings":{"codex":{"web_search":"cached","delegate_model":"synthetic-runner","delegate_fast":true},"agent":{"daily_request_cap":"6","max_tool_calls":6,"task_deadline_seconds":45}}});
    let response = fixture.client.patch(&url).json(&fields).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        body,
        json!({"version":2,"message":"Could not save file; previous file retained"})
    );
    assert_eq!(fixture.snapshot().await, v1);
    assert_eq!(snapshot_v2(&fixture, &url).await, before);
    assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
    fs::remove_dir(&path).unwrap();
    let response = fixture.client.patch(&url).json(&fields).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"version":2,"restart_pending":true})
    );
    let data = snapshot_v2(&fixture, &url).await;
    assert_eq!(data["settings"]["codex"]["delegate_fast"], true);
    assert_eq!(data["restart_pending"], true);
    assert_eq!(data["settings"]["agent"], json!({"daily_request_cap":"6"}));
    assert_eq!(data["settings"]["codex"]["delegate_model"], "synthetic-runner");
    assert_eq!(data["settings"]["codex"]["web_search"], "cached");
    assert_eq!(data["portos"]["link_status"], "unconfigured");
    assert!(data["portos"].get("server").is_none());
    assert_eq!(
        data["settings"]["voice"],
        serde_json::to_value(&v1.settings.voice).unwrap()
    );
    assert_eq!(data["prompts"], serde_json::to_value(&v1.prompts).unwrap());
    assert_eq!(fixture.snapshot().await.settings.agent.max_tool_calls, 8);
    let persisted = fs::read_to_string(&path).unwrap();
    let decoded = SettingsOverlay::from_toml_str(&persisted).unwrap();
    assert_eq!(decoded.legacy_tool_cap(), None);
    assert!(!persisted.contains("task_deadline_seconds"));
    disable_fast_mode(&fixture, &url, &path).await;
    assert_eq!(fs::read(base).unwrap(), original_base);
    owner_only(&path);
    writeln!(
        std::io::stdout().lock(),
        "Refused failed rename with {body}; accepted settings {}; base bytes identical",
        data["settings"],
    )
    .unwrap();
    fixture.close().await;
}

#[path = "settings_http_tests/legacy.rs"]
mod legacy;

async fn disable_fast_mode(fixture: &Fixture, url: &str, path: &Path) {
    use serde_json::{Value, json};
    let response = fixture
        .client
        .patch(url)
        .json(&json!({"version":2,"settings":{"codex":{"delegate_fast":false}}}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"version":2,"restart_pending":true})
    );
    let after = snapshot_v2(fixture, url).await;
    assert_eq!(after["settings"]["codex"]["delegate_fast"], false);
    assert_eq!(after["settings"]["codex"]["delegate_model"], "synthetic-runner");
    assert_eq!(after["settings"]["codex"]["web_search"], "cached");
    assert_eq!(after["settings"]["agent"], json!({"daily_request_cap":"6"}));
    let overlay = fs::read_to_string(path).unwrap();
    assert!(overlay.contains("delegate_fast = false\n"));
}

async fn snapshot_v2(fixture: &Fixture, url: &str) -> serde_json::Value {
    let response = fixture.client.get(url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    response.json().await.unwrap()
}
