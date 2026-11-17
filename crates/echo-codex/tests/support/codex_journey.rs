//! Test-only real process adapter. Cargo builds this once with the integration test.
use echo_codex::{
    DynamicTool, ThreadSettings,
    codex_protocol::{
        Account, AccountRead, AccountReadResult, LoginParams, LoginResult, Message, RealtimeStart, ThreadId,
        ThreadStartResult, ToolCall, ToolContent, ToolResult, TurnInterrupt,
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
            if config["case"] == "drop" {
                return drop_active(supervisor).await;
            }
            let result = tokio::time::timeout(std::time::Duration::from_secs(20), async {
                if !config["case"].as_str().unwrap_or("happy").starts_with("call-")
                    && !config["case"].as_str().unwrap_or("happy").starts_with("sideband-")
                    && !config["case"].as_str().unwrap_or("happy").starts_with("responses-")
                {
                    journey(&mut supervisor, config["case"].as_str().unwrap_or("happy")).await
                } else {
                    failure(&mut supervisor, config["case"].as_str().ok_or("missing case")?).await
                }
            })
            .await;
            supervisor.shutdown().await?;
            let mut report = result??;
            report["reaped"] = json!(supervisor.pid().is_none());
            writeln!(std::io::stdout(), "RUST_REPORT={report}")?;
            Ok(())
        })
}

async fn drop_active(mut supervisor: Supervisor) -> Result {
    sign_in(&mut supervisor).await?;
    let tid = start_thread(&mut supervisor).await?;
    let pid = supervisor.pid().ok_or("missing child")?;
    let io = supervisor.io()?;
    let id = io.begin_request(
        "thread/realtime/start",
        &RealtimeStart::new(&tid, "v=0\r\ns=fake-offer\r\n"),
    )?;
    io.wait_reply(&id).await?;
    loop {
        if let Message::Request { method, params, .. } = io.next_event().await? {
            assert_eq!(method, "item/tool/call");
            assert_eq!(params["threadId"], tid);
            assert_eq!(params["tool"], "echo_test");
            held(pid, "drop")?;
            drop(supervisor);
            tokio::task::yield_now().await;
            writeln!(std::io::stdout(), "RUST_REPORT={}", json!({"dropped":true}))?;
            return Ok(());
        }
    }
}

fn held(pid: u32, expected: &str) -> Result {
    writeln!(std::io::stdout(), "RUST_TOOL_HELD={pid}")?;
    std::io::stdout().flush()?;
    let mut signal = String::new();
    std::io::stdin().lock().read_line(&mut signal)?;
    assert_eq!(signal.trim_end(), expected);
    Ok(())
}

async fn failure(supervisor: &mut Supervisor, case: &str) -> Result<Value> {
    sign_in(supervisor).await?;
    let tid = start_thread(supervisor).await?;
    let io = supervisor.io()?;
    let id = io.begin_request(
        "thread/realtime/start",
        &RealtimeStart::new(&tid, "v=0\r\ns=fake-offer\r\n"),
    )?;
    io.wait_reply(&id).await?;
    if case.starts_with("responses-") {
        return failed_turn(supervisor, &tid).await;
    }
    let mut report = json!({"errors":[], "closed":null, "tools":[], "sdp":[]});
    loop {
        match io.next_event().await? {
            Message::Request { .. } => return Err("unexpected tool request after disconnect".into()),
            Message::Notification { method, params, .. } => match method.as_str() {
                "thread/realtime/error" => {
                    assert_eq!(params["threadId"], tid);
                    report["errors"]
                        .as_array_mut()
                        .ok_or("errors array")?
                        .push(params["message"].clone());
                    if case.starts_with("call-") {
                        let stop = io.begin_request("thread/realtime/stop", &ThreadId { thread_id: tid.clone() })?;
                        io.wait_reply(&stop).await?;
                    }
                }
                "thread/realtime/sdp" => {
                    report["sdp"]
                        .as_array_mut()
                        .ok_or("sdp array")?
                        .push(params["sdp"].clone());
                }
                "thread/realtime/closed" => {
                    assert_eq!(params["threadId"], tid);
                    report["closed"] = params["reason"].clone();
                    return Ok(report);
                }
                _ => {}
            },
            _ => return Err("unexpected response event".into()),
        }
    }
}

async fn failed_turn(supervisor: &mut Supervisor, tid: &str) -> Result<Value> {
    let io = supervisor.io()?;
    let mut report = json!({"errors":[], "closed":null, "tools":[]});
    loop {
        match io.next_event().await? {
            Message::Request { .. } => return Err("unexpected tool request after model refusal".into()),
            Message::Notification { method, params, .. } => match method.as_str() {
                "error" => {
                    assert_eq!(params["threadId"], tid);
                    report["errors"]
                        .as_array_mut()
                        .ok_or("errors array")?
                        .push(params["error"].clone());
                }
                "turn/completed" => {
                    assert_eq!(params["threadId"], tid);
                    assert_eq!(params["turn"]["status"], "failed");
                    report["status"] = params["turn"]["status"].clone();
                    let stop = io.begin_request("thread/realtime/stop", &ThreadId { thread_id: tid.into() })?;
                    io.wait_reply(&stop).await?;
                }
                "thread/realtime/closed" => {
                    assert_eq!(params["threadId"], tid);
                    report["closed"] = params["reason"].clone();
                    return Ok(report);
                }
                _ => {}
            },
            _ => return Err("unexpected response event".into()),
        }
    }
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

async fn journey(supervisor: &mut Supervisor, case: &str) -> Result<Value> {
    sign_in(supervisor).await?;
    let tid = start_thread(supervisor).await?;
    let io = supervisor.io()?;
    let id = io.begin_request(
        "thread/realtime/start",
        &RealtimeStart::new(&tid, "v=0\r\ns=fake-offer\r\n"),
    )?;
    io.wait_reply(&id).await?;
    let mut report = json!({"sdp":[], "tools":[], "started":[], "closed":false, "transcripts":[]});
    delegation(supervisor, &tid, &mut report, case).await?;
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

async fn delegation(supervisor: &mut Supervisor, tid: &str, report: &mut Value, case: &str) -> Result {
    let pid = supervisor.pid().ok_or("missing child")?;
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
                report["turn_id"] = json!(tool.turn_id);
                if case == "survivor" {
                    held(pid, "resume")?;
                }
                if case == "interrupt" {
                    let id = io.begin_request(
                        "turn/interrupt",
                        &TurnInterrupt {
                            thread_id: tid.into(),
                            turn_id: tool.turn_id,
                        },
                    )?;
                    io.wait_reply(&id).await?;
                    continue;
                }
                io.respond(
                    &id,
                    &ToolResult {
                        success: true,
                        content_items: vec![ToolContent::InputText { text: "echo-ok".into() }],
                    },
                )?;
            }
            Message::Notification { method, params, .. } => match method.as_str() {
                "thread/realtime/sdp"
                | "thread/realtime/started"
                | "thread/realtime/transcript/delta"
                | "thread/realtime/transcript/done" => {
                    record_notification(tid, &method, &params, report)?;
                }
                "thread/realtime/error" => return Err("unexpected realtime error".into()),
                "turn/completed" => {
                    verify_completed(tid, case, &params, report);
                    return Ok(());
                }
                _ => {}
            },
            _ => return Err("unexpected response event".into()),
        }
    }
}

fn verify_completed(tid: &str, case: &str, params: &Value, report: &Value) {
    assert_eq!(params["threadId"], tid);
    assert_eq!(
        params["turn"]["status"],
        if case == "interrupt" {
            "interrupted"
        } else {
            "completed"
        }
    );
    assert_eq!(params["turn"]["id"], report["turn_id"]);
}

fn record_notification(tid: &str, method: &str, params: &Value, report: &mut Value) -> Result {
    assert_eq!(params["threadId"], tid);
    let (key, value) = match method {
        "thread/realtime/sdp" => ("sdp", params["sdp"].clone()),
        "thread/realtime/started" => ("started", params["version"].clone()),
        _ => (
            "transcripts",
            json!({"method":method, "role":params["role"],
            "delta":params.get("delta"), "text":params.get("text")}),
        ),
    };
    report[key].as_array_mut().ok_or("report array")?.push(value);
    Ok(())
}
