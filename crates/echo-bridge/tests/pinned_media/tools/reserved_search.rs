//! T3: a delegated model that reserves `web.run` refuses its direct declaration.
//!
//! gpt-6.2-* reserve `web.run`, and the live backend answers a direct
//! declaration of it with HTTP 400 (Codex 0.171.0, measured 2026-11-21). The
//! fake backend models that refusal for a gpt-6.2 model only. The bridge's
//! default delegated model, gpt-6.1-sol, is accepted with search on: the
//! `t3_pinned_tools_native_direct_call_*` witnesses run it with live search.
use super::*;

const RESERVED: &str = "gpt-6.2-sol";

#[tokio::test]
async fn t3_pinned_tools_reserved_model_refuses_direct_web_run() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(exercise()))
        .await
        .unwrap();
}

#[test]
fn t3_pinned_tools_default_delegate_model_is_not_reserved() {
    assert_eq!(echo_config::DEFAULT_DELEGATE_MODEL, "gpt-6.1-sol");
    assert!(!echo_config::DEFAULT_DELEGATE_MODEL.starts_with("gpt-6.2-"));
}

async fn exercise() {
    let directory = tempfile::tempdir().unwrap();
    // The default configuration: live search, so `web` is declared directly.
    let config = CodexConfig {
        binary: Some(binary()),
        home: Some(directory.path().join("home")),
        ..CodexConfig::default()
    };
    let spoof = Spoof::start(echo_spoof_mcp::Options::default()).await.unwrap();
    let (host, catalog) = catalog(&spoof).await;
    let mut provider = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pinned_media/provider.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = provider.stdin.take().unwrap();
    let mut output = BufReader::new(provider.stdout.take().unwrap()).lines();
    let ready: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    let origin = ready["origin"].as_str().unwrap();
    let routes = LocalRoutes::new(origin, origin).unwrap();
    let initialize = json!({"tools":catalog.tools(), "negative":false, "model":RESERVED, "web_search":"live"});
    input.write_all(format!("{initialize}\n").as_bytes()).await.unwrap();
    let (commands, receive) = mpsc::channel(2);
    let (started, model_started) = tokio::sync::oneshot::channel();
    // The refused turn ends with no tool call and no answer, as an
    // interrupted one does; a retried request would be a second refusal.
    let flags = (false, true, model_started, ToolBudget::new(1, || Ok(7)), RESERVED);
    let refusal = json!({"reserved":"web.run", "model":RESERVED});
    let ((), (mut remote, _)) = tokio::join!(
        client(&config, &routes, (host, catalog), commands, flags),
        remote(&mut input, &mut output, receive, started, &refusal)
    );
    remote.close();
    input.write_all(b"{\"snapshot\":true}\n").await.unwrap();
    let value: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    // The delegation began; the model never ran and nothing was handed back.
    assert_eq!(value["delegations"], json!(["delegation-local"]));
    assert_eq!(value["models"], json!([]));
    assert_eq!(value["appends"], json!([]));
    check_mcp(&spoof, true);
    drop(input);
    assert!(provider.wait().await.unwrap().success());
    spoof.shutdown().await.unwrap();
}
