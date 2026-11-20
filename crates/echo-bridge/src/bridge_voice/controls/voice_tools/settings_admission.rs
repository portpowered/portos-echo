use super::*;
use crate::bridge_settings::SettingsService;
use echo_config::{EffectiveConfig, SettingsOverlay};
use echo_screen_contract::settings_v1::{LinkStatusV1, PromptTextsV1};
use reqwest::{Client, StatusCode};
use std::fs;
use std::io::Write as _;

#[tokio::test(start_paused = true)]
async fn t3_settings_v2_live_narrowing() {
    drive(async {
        let scopes = vec!["endpoint:read".into(), "message:send".into()];
        let mut peer = TlsPeer::start_with_catalog(false, scopes, true).await;
        let (service, mut screen, mut tools) = open_paired_portos(&mut peer).await;
        let owner = tools.runtime.as_ref().unwrap();
        relink(owner).await;
        relink(owner).await;
        let catalog = tools.catalog().await.unwrap();
        let grant = tools.grant().unwrap();
        let addr = screen.address().unwrap();
        let url = format!("http://{addr}/settings/v2?token=synthetic-owner");
        let client = Client::builder().no_proxy().build().unwrap();
        let (release, stopped) = oneshot::channel();
        let work = async {
            stopped.await.unwrap();
        };
        let inspect = async {
            exercise_running_catalog(tools.host.as_ref().unwrap(), &client, &url, &peer.config).await;
            release.send(()).unwrap();
        };
        let (((), completion), ()) = tokio::join!(
            service.scope_with_grant(work, || LinkStatusV1::Linked, || grant.granted_scopes()),
            inspect
        );
        assert!(completion.restart_requested);
        assert_eq!(tools.catalog().await.unwrap().tools(), catalog.tools());
        for name in ["control", "endpoint_show", "endpoint_query"] {
            assert_eq!(catalog.resolve(name), Some(name));
        }
        assert_eq!(catalog.resolve("group_manage"), None);
        let original = serde_json::to_value(catalog.tools()).unwrap();
        tools.close().await;
        screen.close().await.unwrap();
        let counts = peer.finish().await;
        assert_eq!(counts, json!({"link":2,"token":2,"catalog":1,"query":1}));
        writeln!(
            std::io::stdout().lock(),
            "Unchanged catalog {original}; outbound calls {counts}; control=0 endpoint_show=0 endpoint_query=1"
        )
        .unwrap();
    })
    .await;
}

async fn open_paired_portos(peer: &mut TlsPeer) -> (SettingsService, Screen, Tools) {
    let root = peer.directory.path();
    peer.config.home.as_mut().unwrap().dir = root.join("home");
    peer.config.screen.listen = ([127, 0, 0, 1], 0).into();
    let overlay = SettingsOverlay::from_toml_str("screen_pack='template'\n[voice]\nsession_listen_seconds=20").unwrap();
    let (service, send) = SettingsService::open(
        EffectiveConfig {
            base: peer.config.clone(),
            config: peer.config.clone(),
            overlay,
            warning: None,
        },
        PromptTextsV1 {
            voice: "synthetic voice".into(),
            runner: "synthetic runner".into(),
        },
    )
    .unwrap();
    let screen = Screen::bind_with_settings(Some(&peer.config.screen), Some(send))
        .await
        .unwrap();
    let mut trace = Trace::open(&root.join("trace"), "api_key").unwrap();
    let mut tools = Tools::new(&peer.config, Arc::new(|| 0)).with_portos_permissions(Some(service.permissions()));
    tools.open(&peer.config, &screen, &mut trace, None).await.unwrap();
    (service, screen, tools)
}

async fn exercise_running_catalog(host: &impl ToolHost, client: &Client, url: &str, config: &BridgeConfig) {
    assert_narrow_save(client, url, config).await;
    assert_eq!(
        host.call_tool("control", json!({}), "1:t1:u1:a").await,
        Err(ToolError::Denied)
    );
    assert_eq!(
        host.call_tool("endpoint_show", json!({}), "1:t1:u1:b").await,
        Err(ToolError::Denied)
    );
    assert_wider_save(client, url).await;
    assert_eq!(
        host.call_tool("control", json!({}), "1:t1:u1:d").await,
        Err(ToolError::Denied)
    );
    assert_eq!(
        host.call_tool("endpoint_show", json!({}), "1:t1:u1:e").await,
        Err(ToolError::Denied)
    );
    let result = host.call_tool("endpoint_query", json!({}), "1:t1:u1:c").await.unwrap();
    assert_eq!(result.content, json!([{"type":"text","text":"synthetic-query-result"}]));
    assert!(!result.is_error);
}

async fn assert_narrow_save(client: &Client, url: &str, config: &BridgeConfig) {
    let overlay = config.home.as_ref().unwrap().dir.join("settings.toml");
    let initial = get(client, url).await;
    assert_eq!(initial["portos"]["link_status"], "linked");
    assert_eq!(
        initial["portos"]["server"]["requested_scopes"],
        json!(["endpoint:read", "message:send"])
    );
    assert_eq!(
        initial["portos"]["server"]["granted_scopes"],
        json!(["endpoint:read", "message:send"])
    );
    assert_eq!(initial["settings"]["voice"]["session_listen_seconds"], 20);
    let patch = json!({
        "codex":{"web_search":"cached","delegate_model":"synthetic-runner"},
        "agent":{"max_tool_calls":6},
        "portos":{"scopes":["endpoint:read"],"allow_tools":["endpoint_query"]}
    });
    save(client, url, &patch).await;
    let saved = get(client, url).await;
    assert_eq!(saved["settings"]["codex"]["web_search"], "cached");
    assert_eq!(saved["settings"]["codex"]["delegate_model"], "synthetic-runner");
    assert_eq!(saved["settings"]["agent"]["max_tool_calls"], 6);
    assert_eq!(saved["portos"]["server"]["requested_scopes"], json!(["endpoint:read"]));
    assert_eq!(
        saved["portos"]["server"]["granted_scopes"],
        json!(["endpoint:read", "message:send"])
    );
    assert_eq!(saved["portos"]["server"]["allow_tools"], json!(["endpoint_query"]));
    assert_eq!(saved["portos"]["link_waiting_for_restart"], true);
    assert_eq!(saved["prompts"], initial["prompts"]);
    let file = fs::read_to_string(&overlay).unwrap();
    let decoded = SettingsOverlay::from_toml_str(&file).unwrap();
    let effective = decoded.apply_to(config).unwrap();
    assert_eq!(decoded.screen_pack(), Some("template"));
    assert_eq!(effective.voice.session_listen_seconds, 20);
    assert_eq!(effective.codex.web_search, echo_config::WebSearch::Cached);
    assert_eq!(effective.codex.delegate_model.as_deref(), Some("synthetic-runner"));
    assert_eq!(effective.agent.max_tool_calls, 6);
    assert_eq!(effective.mcp.scopes, ["endpoint:read"]);
    assert_eq!(effective.mcp.server[0].allow_tools, ["endpoint_query"]);
    assert_private(&overlay);
    writeln!(
        std::io::stdout().lock(),
        "Saved settings.toml (0600) {file}\nNarrow status {}",
        saved["portos"]
    )
    .unwrap();
}

async fn assert_wider_save(client: &Client, url: &str) {
    save(
        client,
        url,
        &json!({"portos":{
            "scopes":["endpoint:read","message:send","group:manage"],
            "allow_tools":["endpoint_query","control","endpoint_show","group_manage"]
        }}),
    )
    .await;
    let wider = get(client, url).await;
    assert_eq!(wider["portos"]["link_status"], "relink_for_scope");
    assert_eq!(
        wider["portos"]["server"]["requested_scopes"],
        json!(["endpoint:read", "message:send", "group:manage"])
    );
    assert_eq!(
        wider["portos"]["server"]["granted_scopes"],
        json!(["endpoint:read", "message:send"])
    );
    writeln!(std::io::stdout().lock(), "Wider status {}", wider["portos"]).unwrap();
}

async fn get(client: &Client, url: &str) -> Value {
    let reply = client.get(url).send().await.unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    reply.json().await.unwrap()
}

async fn save(client: &Client, url: &str, patch: &Value) {
    let reply = client
        .patch(url)
        .json(&json!({"version":2,"settings":patch}))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(
        reply.json::<Value>().await.unwrap(),
        json!({"version":2,"restart_pending":true})
    );
}
