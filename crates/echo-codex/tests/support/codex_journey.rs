//! Test-only real process adapter. Cargo builds this once with the integration test.
use echo_codex::{
    DynamicTool, ThreadSettings,
    codex_protocol::{
        Account, AccountRead, AccountReadResult, LoginParams, LoginResult, Message, RealtimeStart, ThreadId,
        ThreadStartResult, ToolCall, ToolContent, ToolResult,
    },
    codex_supervisor::{Supervisor, SupervisorConfig},
};
use echo_contracts::secret::Secret;
use serde_json::{Value, json};
use std::{
    error::Error,
    io::{BufRead, Write},
    path::PathBuf,
};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

pub fn run(config: &str) -> Result {
    let config: Value = serde_json::from_str(config)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let mut supervisor = configure(&config)?;
            supervisor.start().await?;
            let result = journey(&mut supervisor).await;
            supervisor.shutdown().await?;
            let mut report = result?;
            report["reaped"] = json!(supervisor.pid().is_none());
            writeln!(std::io::stdout(), "RUST_REPORT={report}")?;
            Ok(())
        })
}

fn configure(config: &Value) -> Result<Supervisor> {
    let text = |key: &str| config[key].as_str().ok_or("missing fixture setting");
    let mut settings = SupervisorConfig::new(PathBuf::from(text("binary")?), PathBuf::from(text("home")?));
    settings.config_overrides = serde_json::from_value(config["overrides"].clone())?;
    let mut supervisor = Supervisor::new(settings)?;
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        supervisor.set_environment(key, text("base")?.into())?;
    }
    for key in ["NO_PROXY", "no_proxy"] {
        supervisor.set_environment(key, "127.0.0.1".into())?;
    }
    Ok(supervisor)
}

async fn sign_in(supervisor: &mut Supervisor) -> Result {
    let io = supervisor.io()?;
    let id = io.begin_request(
        "account/read",
        &AccountRead {
            refresh_token: Some(false),
        },
    )?;
    let before: AccountReadResult = serde_json::from_value(io.wait_reply(&id).await?)?;
    assert!(before.account.is_none());
    let key = Secret::new("dummy-local-only".to_owned());
    let id = io.begin_request("account/login/start", &LoginParams::ApiKey { api_key: &key })?;
    let login: LoginResult = serde_json::from_value(io.wait_reply(&id).await?)?;
    assert!(matches!(login, LoginResult::ApiKey));
    let id = io.begin_request(
        "account/read",
        &AccountRead {
            refresh_token: Some(false),
        },
    )?;
    let raw = io.wait_reply(&id).await?;
    let after: AccountReadResult = serde_json::from_value(raw)?;
    assert!(matches!(after.account, Some(Account::ApiKey)));
    Ok(())
}

async fn start_thread(supervisor: &mut Supervisor) -> Result<String> {
    let io = supervisor.io()?;
    let settings = ThreadSettings::new(vec![DynamicTool {
        name: "echo_test".into(),
        description: "Synthetic echo".into(),
        input_schema: json!({"type":"object", "properties":{"message":{"type":"string"}},
            "required":["message"], "additionalProperties":false}),
    }]);
    let id = io.begin_request("thread/start", &settings)?;
    let thread: ThreadStartResult = serde_json::from_value(io.wait_reply(&id).await?)?;
    Ok(thread.thread.id)
}

async fn journey(supervisor: &mut Supervisor) -> Result<Value> {
    sign_in(supervisor).await?;
    let tid = start_thread(supervisor).await?;
    let io = supervisor.io()?;
    let id = io.begin_request(
        "thread/realtime/start",
        &RealtimeStart::new(&tid, "v=0\r\ns=fake-offer\r\n"),
    )?;
    io.wait_reply(&id).await?;
    let mut report = json!({"sdp":[], "tools":[], "started":[], "closed":false});
    delegation(supervisor, &tid, &mut report).await?;
    writeln!(std::io::stdout(), "RUST_TURN_COMPLETE")?;
    std::io::stdout().flush()?;
    let mut signal = String::new();
    std::io::stdin().lock().read_line(&mut signal)?;
    assert_eq!(signal.trim_end(), "append-observed");
    let io = supervisor.io()?;
    let id = io.begin_request("thread/realtime/stop", &ThreadId { thread_id: tid.clone() })?;
    io.wait_reply(&id).await?;
    loop {
        if let Message::Notification { method, params, .. } = io.next_event().await? {
            if method == "thread/realtime/closed" {
                assert_eq!(params["threadId"], tid);
                report["closed"] = json!(true);
                break;
            }
            assert_ne!(method, "thread/realtime/error");
        }
    }
    Ok(report)
}

async fn delegation(supervisor: &mut Supervisor, tid: &str, report: &mut Value) -> Result {
    let io = supervisor.io()?;
    loop {
        match io.next_event().await? {
            Message::Request { id, method, params } => {
                assert_eq!(method, "item/tool/call");
                let tool: ToolCall = serde_json::from_value(params.clone())?;
                assert_eq!(tool.thread_id, tid);
                assert_eq!(tool.tool, "echo_test");
                assert_eq!(tool.arguments, json!({"message":"ping"}));
                report["tools"].as_array_mut().ok_or("tools array")?.push(params);
                io.respond(
                    &id,
                    &ToolResult {
                        success: true,
                        content_items: vec![ToolContent::InputText { text: "echo-ok".into() }],
                    },
                )?;
            }
            Message::Notification { method, params, .. } => match method.as_str() {
                "thread/realtime/sdp" | "thread/realtime/started" => {
                    assert_eq!(params["threadId"], tid);
                    let (key, field) = if method.ends_with("/sdp") {
                        ("sdp", "sdp")
                    } else {
                        ("started", "version")
                    };
                    report[key]
                        .as_array_mut()
                        .ok_or("report array")?
                        .push(params[field].clone());
                }
                "thread/realtime/error" => return Err("unexpected realtime error".into()),
                "turn/completed" => {
                    assert_eq!(params["threadId"], tid);
                    assert_eq!(params["turn"]["status"], "completed");
                    return Ok(());
                }
                _ => {}
            },
            _ => return Err("unexpected response event".into()),
        }
    }
}
