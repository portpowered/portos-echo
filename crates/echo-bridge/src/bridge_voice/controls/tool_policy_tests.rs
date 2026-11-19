use super::*;
use echo_screen_contract::{UiAction, UiState};
use serde_json::json;
use tokio::sync::mpsc;

#[derive(Default)]
struct Host(Mutex<Vec<(String, Value, String)>>);
impl ToolHost for Host {
    fn list_tools(&self) -> impl std::future::Future<Output = Result<Vec<ToolSpec>, ToolError>> {
        std::future::ready(Ok(vec![]))
    }
    async fn call_tool(&self, name: &str, args: Value, key: &str) -> Result<ToolResult, ToolError> {
        self.0.lock().unwrap().push((name.into(), args, key.into()));
        if name == "test.wait" {
            std::future::pending::<()>().await;
        }
        Ok(ToolResult {
            content: json!([{"type":"text","text":"done"}]),
            is_error: false,
        })
    }
}

fn policy(directory: &Path) -> (Policy<Host>, mpsc::Receiver<UiState>) {
    let (send, receive) = mpsc::channel(32);
    let host = Policy::new(
        Host::default(),
        &["test.echo".into(), "test.wait".into(), "test.requires_approval".into()],
        directory,
        Approvals::default(),
        Presentation::new(Some(send)),
    )
    .unwrap();
    (host, receive)
}

#[tokio::test(start_paused = true)]
async fn t3_mcp_receipts_are_absent_for_spoof_catalog_and_tool_returns() {
    use super::super::voice_tools::Tools;
    use crate::{bridge_runtime::bridge_screen::Screen, bridge_trace::Trace};
    let directory = tempfile::tempdir().unwrap();
    let spoof = echo_spoof_mcp::Spoof::start(echo_spoof_mcp::Options::default())
        .await
        .unwrap();
    let mut config =
        echo_config::BridgeConfig::from_toml_str(include_str!("../../../../../examples/bridge.example.toml")).unwrap();
    config.mcp.server[0].url = spoof.url().into();
    config.audit.log_dir = directory.path().join("audit");
    let path = directory.path().join("trace");
    let mut trace = Trace::open(&path, "api_key").unwrap();
    let screen = Screen::bind(None).await.unwrap();
    let mut tools = Tools::new(&config, Arc::new(|| 0));
    tools.open(&config, &screen, &mut trace, None).await.unwrap();
    tools.catalog().await.unwrap();
    let result = tools
        .host
        .as_ref()
        .unwrap()
        .call_tool("test.echo", json!({"text":"synthetic-spoof-result"}), "1:t1:u1:c")
        .await
        .unwrap();
    assert_eq!(result.content, json!([{"type":"text","text":"synthetic-spoof-result"}]));
    assert!(!result.is_error);
    tools.close().await;
    let raw = std::fs::read_to_string(path).unwrap();
    let rows: Vec<Value> = raw.lines().map(|row| serde_json::from_str(row).unwrap()).collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["event"], "tools/call");
    assert_eq!(raw.matches("synthetic-spoof-result").count(), 0);
    assert_eq!(spoof.snapshot().unwrap().calls.len(), 1);
    spoof.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn policy_reserves_once_before_effect_and_reopen_never_replays() {
    let directory = tempfile::tempdir().unwrap();
    let (host, _screen) = policy(directory.path());
    let args = json!({"text":"private argument fixture"});
    assert_eq!(
        host.call_tool("unlisted", args.clone(), "unlisted").await,
        Err(ToolError::Denied)
    );
    assert_eq!(
        host.call_tool("test.echo", json!([]), "invalid").await,
        Err(ToolError::Denied)
    );
    assert!(
        !host
            .call_tool("test.echo", args.clone(), "effect-1")
            .await
            .unwrap()
            .is_error
    );
    assert_eq!(
        host.call_tool("test.echo", args.clone(), "effect-1").await,
        Err(ToolError::Denied)
    );
    assert_eq!(
        *host.0.host.0.lock().unwrap(),
        [("test.echo".into(), args.clone(), "effect-1".into())]
    );
    assert!(matches!(
        Policy::new(
            Host::default(),
            &[],
            directory.path(),
            Approvals::default(),
            Presentation::new(None)
        ),
        Err(ToolError::Transport)
    ));
    drop(host);
    let (host, _) = policy(directory.path());
    assert_eq!(
        host.call_tool("test.echo", args.clone(), "effect-1").await,
        Err(ToolError::Denied)
    );
    assert!(host.0.host.0.lock().unwrap().is_empty());
    assert!(!host.call_tool("test.echo", args, "effect-2").await.unwrap().is_error);
    assert_eq!(host.0.host.0.lock().unwrap().len(), 1);
    drop(host);
    assert!(
        !std::fs::read_to_string(directory.path().join("codex-tool-ledger.jsonl"))
            .unwrap()
            .contains("private argument fixture")
    );
}

#[tokio::test(start_paused = true)]
async fn policy_approval_requires_matching_current_action_and_denial_has_zero_effects() {
    let directory = tempfile::tempdir().unwrap();
    let (host, mut screen) = policy(directory.path());
    for accepted in [false, true] {
        let key = if accepted { "accepted" } else { "denied" };
        let call = host.call_tool("test.requires_approval", json!({}), key);
        let operator = async {
            let UiState::Approval { id, prompt, .. } = screen.recv().await.unwrap() else {
                unreachable!()
            };
            assert_eq!(prompt, "Allow test.requires_approval?");
            host.0
                .approvals
                .answer(&UiAction::Approve { id: "stale".into() })
                .unwrap();
            assert!(host.0.host.0.lock().unwrap().is_empty());
            let action = if accepted {
                UiAction::Approve { id: id.clone() }
            } else {
                UiAction::Deny { id: id.clone() }
            };
            host.0.approvals.answer(&action).unwrap();
            host.0.approvals.answer(&UiAction::Approve { id }).unwrap();
        };
        let (result, ()) = tokio::join!(call, operator);
        if accepted {
            assert!(!result.unwrap().is_error);
        } else {
            assert_eq!(result, Err(ToolError::Denied));
        }
        assert!(matches!(screen.recv().await.unwrap(), UiState::Ready { .. }));
    }
    assert_eq!(
        *host.0.host.0.lock().unwrap(),
        [("test.requires_approval".into(), json!({}), "accepted".into())]
    );
}

#[tokio::test(start_paused = true)]
async fn policy_cancelled_consent_retires_prompt_and_cannot_authorize_later_work() {
    let directory = tempfile::tempdir().unwrap();
    let (host, mut screen) = policy(directory.path());
    let mut call = Box::pin(host.call_tool("test.requires_approval", json!({}), "cancelled"));
    let UiState::Approval { id: old, .. } = (tokio::select! {
        result = &mut call => unreachable!("unexpected {result:?}"),
        state = screen.recv() => state.unwrap(),
    }) else {
        unreachable!()
    };
    drop(call);
    assert!(matches!(screen.recv().await.unwrap(), UiState::Ready { .. }));
    host.0.approvals.answer(&UiAction::Approve { id: old.clone() }).unwrap();
    assert_eq!(
        host.call_tool("test.requires_approval", json!({}), "cancelled").await,
        Err(ToolError::Denied)
    );
    let call = host.call_tool("test.requires_approval", json!({}), "new");
    let operator = async {
        let UiState::Approval { id, .. } = screen.recv().await.unwrap() else {
            unreachable!()
        };
        assert_ne!(id, old);
        host.0.approvals.answer(&UiAction::Approve { id: old }).unwrap();
        assert!(host.0.host.0.lock().unwrap().is_empty());
        host.0.approvals.answer(&UiAction::Approve { id }).unwrap();
    };
    let (result, ()) = tokio::join!(call, operator);
    assert!(!result.unwrap().is_error);
    assert_eq!(host.0.host.0.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn policy_cancelled_effect_and_corrupt_ledger_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let (host, _) = policy(directory.path());
    let mut call = Box::pin(host.call_tool("test.wait", json!({}), "ambiguous"));
    tokio::select! {
        result = &mut call => unreachable!("unexpected {result:?}"),
        () = tokio::task::yield_now() => (),
    }
    assert_eq!(host.0.host.0.lock().unwrap().len(), 1);
    drop(call);
    drop(host);
    let (host, _) = policy(directory.path());
    assert_eq!(
        host.call_tool("test.wait", json!({}), "ambiguous").await,
        Err(ToolError::Denied)
    );
    assert!(host.0.host.0.lock().unwrap().is_empty());
    drop(host);
    std::fs::write(directory.path().join("codex-tool-ledger.jsonl"), b"\"partial").unwrap();
    assert!(matches!(
        Policy::new(
            Host::default(),
            &[],
            directory.path(),
            Approvals::default(),
            Presentation::new(None)
        ),
        Err(ToolError::Transport)
    ));
}

#[tokio::test(start_paused = true)]
async fn policy_cancelled_prompt_preserves_relink_and_hidden_requests_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let (host, mut screen) = policy(directory.path());
    let mut call = Box::pin(host.call_tool("test.requires_approval", json!({}), "cancelled"));
    tokio::select! {
        result = &mut call => unreachable!("unexpected {result:?}"),
        state = screen.recv() => assert!(matches!(state, Some(UiState::Approval { .. }))),
    }
    let relink = UiState::Error {
        message: "Relink fixture".into(),
        relink: true,
    };
    host.0.presentation.overlay(Some(relink.clone())).unwrap();
    assert_eq!(screen.recv().await.unwrap(), relink);
    drop(call);
    assert!(screen.try_recv().is_err());
    assert_eq!(
        host.call_tool("test.requires_approval", json!({}), "hidden").await,
        Err(ToolError::Denied)
    );
    host.0.presentation.refresh().unwrap();
    assert_eq!(screen.recv().await.unwrap(), relink);
    assert!(host.0.host.0.lock().unwrap().is_empty());
}
