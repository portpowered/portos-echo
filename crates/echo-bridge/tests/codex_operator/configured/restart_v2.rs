//! T3: paired V2 save and grant operations across a delivered bridge replacement.
use super::{restart, *};
use echo_lab::lab_mcp::Fixture as MockPortos;

#[tokio::test]
#[ignore = "T3 requires prebuilt Linux echo-bridge, local Python and echo-lab OAuth"]
async fn t3_settings_v2_restart_installs_overlay_and_links_with_saved_scopes() {
    let fixture = Fixture::new("chatgpt");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let epoch = std::time::UNIX_EPOCH.elapsed().unwrap().as_secs();
    let mock = MockPortos::bind(epoch).await.unwrap();
    let base = configure(&fixture, listener.local_addr().unwrap().port(), mock.base());
    let home = fixture.directory.path().join("agent-home");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("settings.toml"), "[voice]\nsession_listen_seconds = 4\n").unwrap();
    let mut process = Process::spawn(&mut super::super::voice_entry::command(&fixture));
    let pid = process.child.id().unwrap();
    let mut old_satellite = restart::binary_ready(&listener, &mut process).await;
    let identity = restart::process_identity(pid);
    let url = settings_runtime_url(&fixture, "/settings/v2");
    let client = reqwest::Client::new();
    let before = leaves(&snapshot(&client, &url).await);
    assert_eq!(
        before,
        json!([
            "live",
            "original-runner",
            8,
            ["endpoint:read", "message:send"],
            ["endpoint_query", "control"]
        ])
    );
    save(&client, &url).await;
    let replacement = restart::binary_ready(&listener, &mut process).await;
    assert_eq!(process.child.id(), Some(pid));
    assert_eq!(restart::process_identity(pid), identity);
    tokio::time::timeout(OBSERVATION_TIMEOUT, old_satellite.read_to_end(&mut vec![]))
        .await
        .unwrap()
        .unwrap();
    let active = snapshot(&client, &url).await;
    assert_eq!(active["restart_pending"], false);
    let after = leaves(&active);
    assert_eq!(
        after,
        json!(["cached", "synthetic-runner", 6, ["endpoint:read"], ["endpoint_query"]])
    );
    assert_eq!(active["settings"]["voice"]["session_listen_seconds"], 4);
    overlay(&fixture, &home, &base);
    tokio::time::timeout(
        OBSERVATION_TIMEOUT,
        super::super::settings_link::replacement_grant(&fixture, &mock, epoch),
    )
    .await
    .unwrap();
    restart::binary_finished(&fixture, &mut process).await;
    mock.shutdown().await.unwrap();
    drop(replacement);
    writeln!(std::io::stdout().lock(),
        "Settings V2: before={before} after={after} ready_generations=2 same_pid=true mode=0600 temp_siblings=0 base_bytes_equal=true links=1 revokes=1 grant_present=false shutdown=0"
    ).unwrap();
}

async fn save(client: &reqwest::Client, url: &reqwest::Url) {
    let receipt = client
        .patch(url.clone())
        .json(&json!({
            "version":2,
            "settings":{
                "codex":{"web_search":"cached","delegate_model":"synthetic-runner"},
                "agent":{"max_tool_calls":6},
                "portos":{"scopes":["endpoint:read"],"allow_tools":["endpoint_query"]}
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(receipt.status(), reqwest::StatusCode::OK);
    assert_eq!(
        receipt.json::<Value>().await.unwrap(),
        json!({"version":2,"restart_pending":true})
    );
}

fn configure(fixture: &Fixture, port: u16, issuer: &str) -> Vec<u8> {
    restart::binary_home(fixture, port);
    let path = fixture.directory.path().join("bridge.toml");
    let grant = fixture.directory.path().join("grant");
    let mcp = format!(
        r#"client_id = "echo-lab"
state_dir = {grant:?}
operator_listen = "127.0.0.1:0"
scopes = ["endpoint:read", "message:send"]

[[mcp.server]]
name = "local"
url = "{issuer}/mcp"
auth = "portos_device_code"
allow_tools = ["endpoint_query", "control"]"#
    );
    let base = std::fs::read_to_string(&path)
        .unwrap()
        .replace("server = []", &mcp)
        .replace(
            "[codex]\n",
            "[codex]\nweb_search = 'live'\ndelegate_model = 'original-runner'\n",
        )
        .into_bytes();
    std::fs::write(path, &base).unwrap();
    base
}

async fn snapshot(client: &reqwest::Client, url: &reqwest::Url) -> Value {
    let response = client.get(url.clone()).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let value = response.json::<Value>().await.unwrap();
    assert_eq!(value["version"], 2);
    value
}

fn leaves(snapshot: &Value) -> Value {
    json!([
        snapshot["settings"]["codex"]["web_search"],
        snapshot["settings"]["codex"]["delegate_model"],
        snapshot["settings"]["agent"]["max_tool_calls"],
        snapshot["portos"]["server"]["requested_scopes"],
        snapshot["portos"]["server"]["allow_tools"],
    ])
}

fn overlay(fixture: &Fixture, home: &Path, base: &[u8]) {
    assert_eq!(
        std::fs::read(fixture.directory.path().join("bridge.toml")).unwrap(),
        base
    );
    let path = home.join("settings.toml");
    restart::owner_only(&path);
    let decode = |text: &str| echo_config::SettingsOverlay::from_toml_str(text).unwrap();
    let expected = decode(
        r#"[codex]
web_search = "cached"
delegate_model = "synthetic-runner"
[agent]
max_tool_calls = 6
[mcp]
scopes = ["endpoint:read"]
[[mcp.server]]
name = "local"
allow_tools = ["endpoint_query"]
[voice]
session_listen_seconds = 4"#,
    );
    assert_eq!(decode(&std::fs::read_to_string(path).unwrap()), expected);
    for entry in std::fs::read_dir(home).unwrap() {
        let name = entry.unwrap().file_name();
        assert!(!name.to_string_lossy().starts_with(".screen-save-"));
    }
}
