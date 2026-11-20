//! T3: paired V2 save and grant operations across a delivered bridge replacement.
use super::{restart, *};
use echo_lab::lab_mcp::Fixture as MockPortos;

const ACCOUNT_OVERLAY: &str = "[agent]\nmax_tool_calls = 8\n";

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

#[tokio::test]
#[ignore = "T3 requires prebuilt Linux echo-bridge, local Python and echo-lab OAuth"]
async fn t3_settings_account_changes_reuse_the_credential_owners() {
    let fixture = Fixture::new("chatgpt");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let epoch = 1_800_000_000;
    let mock = MockPortos::bind(epoch).await.unwrap();
    let base = configure(&fixture, listener.local_addr().unwrap().port(), mock.base());
    let base = String::from_utf8(base).unwrap().replace(
        r#"scopes = ["endpoint:read", "message:send"]"#,
        r#"scopes = ["endpoint:read"]"#,
    );
    std::fs::write(fixture.directory.path().join("bridge.toml"), base).unwrap();
    let overlay = fixture.directory.path().join("agent-home/settings.toml");
    std::fs::create_dir(overlay.parent().unwrap()).unwrap();
    std::fs::write(&overlay, ACCOUNT_OVERLAY).unwrap();
    std::fs::write(fixture.home.join("mode"), "screen-account").unwrap();
    std::fs::write(fixture.home.join("auth.json"), "synthetic-private-openai-value").unwrap();
    let mut process = account_process(&fixture, &mock, epoch);
    let mut first = restart::binary_ready(&listener, &mut process).await;
    let url = settings_runtime_url(&fixture, "/settings/v2/openai");
    let client = reqwest::Client::new();
    assert_fixed_clock_save_refused(&fixture, &client, &overlay).await;
    assert_eq!(
        snapshot(&client, &url).await,
        json!({"version":2,"link_status":"linked","operation_state":"idle"})
    );
    unlink_openai(&client, &url, listener.local_addr().unwrap().port()).await;
    let signed_out = wait_account(&client, &url, |state| state["link_status"] == "link_required").await;
    assert_eq!(
        signed_out,
        json!({"version":2,"link_status":"link_required","operation_state":"idle"})
    );
    assert!(!fixture.home.join("auth.json").exists());
    tokio::time::timeout(OBSERVATION_TIMEOUT, first.read_to_end(&mut vec![]))
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        OBSERVATION_TIMEOUT,
        super::super::settings_link::controlled_grant(&fixture, &mock, epoch, &mut process),
    )
    .await
    .unwrap();
    link_openai(&fixture, &client, &url).await;
    let second = restart::binary_ready(&listener, &mut process).await;
    assert_eq!(
        snapshot(&client, &url).await,
        json!({"version":2,"link_status":"linked","operation_state":"idle"})
    );
    assert!(fixture.home.join("auth.json").is_file());
    drop(process.input.take());
    process.collect(0).await;
    assert_eq!(process.stderr().await, "");
    drop(second);
    super::super::host::released(&fixture).await;
    mock.shutdown().await.unwrap();
    assert_account_owner_receipts(&fixture);
}

async fn assert_fixed_clock_save_refused(fixture: &Fixture, client: &reqwest::Client, overlay: &Path) {
    let v2 = settings_runtime_url(fixture, "/settings/v2");
    let before = snapshot(client, &v2).await;
    assert_eq!(before["restart_pending"], false);
    assert_eq!(before["settings"]["agent"]["max_tool_calls"], 8);
    for (request, version) in [
        (client.patch(v2.clone()), 2),
        (client.post(settings_runtime_url(fixture, "/settings/v1")), 1),
    ] {
        let response = request
            .json(&json!({"version":version,"settings":{"agent":{"max_tool_calls":6}}}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.bytes().await.unwrap().as_ref(), b"");
        assert_eq!(std::fs::read(overlay).unwrap(), ACCOUNT_OVERLAY.as_bytes());
        assert_eq!(snapshot(client, &v2).await, before);
    }
    let noop = client
        .patch(v2)
        .json(&json!({"version":2,"settings":{"agent":{"max_tool_calls":8}}}))
        .send()
        .await
        .unwrap();
    assert_eq!(noop.status(), reqwest::StatusCode::OK);
    assert_eq!(
        noop.json::<Value>().await.unwrap(),
        json!({"version":2,"restart_pending":false})
    );
    writeln!(
        std::io::stdout().lock(),
        "Fixed clock: V2 PATCH=503 V1 POST=503 overlay_unchanged=true max_tool_calls=8 restart_pending=false noop=200"
    )
    .unwrap();
}

fn account_process(fixture: &Fixture, mock: &MockPortos, epoch: u64) -> Process {
    Process::spawn(super::super::voice_entry::command(fixture).args([
        "--live-loopback",
        mock.base(),
        "--model-loopback",
        mock.base(),
        "--sim-clock",
        "--sim-epoch",
        &epoch.to_string(),
    ]))
}

fn assert_account_owner_receipts(fixture: &Fixture) {
    let methods = std::fs::read_to_string(fixture.home.join("methods")).unwrap();
    let operations: Vec<_> = methods
        .lines()
        .filter(|method| matches!(*method, "account/logout" | "account/login/start"))
        .collect();
    assert_eq!(operations, ["account/logout", "account/login/start"]);
    let rows = trace(&fixture.directory.path().join("trace"));
    for event in ["settings.reexec_pending", "settings.restart_prepared"] {
        assert_eq!(rows.iter().filter(|row| row["event"] == event).count(), 0);
    }
    assert_eq!(
        rows.iter()
            .filter(|row| row["event"] == "mcp.operator.listening")
            .count(),
        1
    );
    let trace = serde_json::to_string(&rows).unwrap();
    for private in [
        "synthetic-private-openai-value",
        "synthetic@example.invalid",
        "local-login",
        "TEST-CODE",
    ] {
        assert!(!trace.contains(private));
    }
    writeln!(std::io::stdout().lock(),
        "OpenAI: linked -> link_required -> linked, logout=1 login=1, Wyoming generations=2; Port OS link/revoke while OpenAI signed out, one grant owner"
    ).unwrap();
}

async fn unlink_openai(client: &reqwest::Client, url: &reqwest::Url, wyoming_port: u16) {
    let unlink = |confirm| {
        client
            .post(url.clone())
            .json(&json!({"version":2,"operation":"unlink","confirm":confirm}))
    };
    assert_eq!(
        unlink(false).send().await.unwrap().status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    let mut foreign = url.clone();
    foreign.set_port(Some(wyoming_port)).unwrap();
    assert_ne!(foreign.origin(), url.origin());
    assert_eq!(
        unlink(true)
            .header("origin", foreign.origin().ascii_serialization())
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let receipt = unlink(true).send().await.unwrap();
    assert_eq!(receipt.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(
        receipt.json::<Value>().await.unwrap(),
        json!({"version":2,"operation":"unlink"})
    );
}

async fn link_openai(fixture: &Fixture, client: &reqwest::Client, url: &reqwest::Url) {
    let start = || client.post(url.clone()).json(&json!({"version":2,"operation":"link"}));
    let receipt = start().send().await.unwrap();
    assert_eq!(receipt.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(
        receipt.json::<Value>().await.unwrap(),
        json!({"version":2,"operation":"link"})
    );
    let value = wait_account(client, url, |state| state.get("challenge").is_some()).await;
    assert_eq!(value["operation_state"], "linking");
    let expires = value["challenge"]["expires_at_unix"].as_u64().unwrap();
    assert!((295..=300).contains(&(expires - std::time::UNIX_EPOCH.elapsed().unwrap().as_secs())));
    assert_eq!(
        value["challenge"],
        json!({
            "verification_uri":"http://127.0.0.1:12345/verify","user_code":"TEST-CODE","expires_at_unix":expires
        })
    );
    assert_eq!(start().send().await.unwrap().status(), reqwest::StatusCode::CONFLICT);
    fixture.control().await.write_all(b"success").await.unwrap();
}

async fn wait_account(client: &reqwest::Client, url: &reqwest::Url, ready: impl Fn(&Value) -> bool) -> Value {
    tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        let mut interval = tokio::time::interval(crate::POLL_INTERVAL);
        loop {
            interval.tick().await;
            let value = snapshot(client, url).await;
            if ready(&value) {
                return value;
            }
        }
    })
    .await
    .unwrap()
}
