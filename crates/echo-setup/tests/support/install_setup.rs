//! Isolated native CLI fixture. Real loopback HTTP, no internet or device tools.
use super::*;

#[cfg(unix)]
#[path = "cancel.rs"]
mod cancel;

type CliLines = tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>;

#[path = "../a2/service.rs"]
mod service;
pub(crate) use service::Service;

async fn settings_reply(listener: &tokio::net::TcpListener) -> Result {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut socket, _) = listener.accept().await?;
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        socket.read_exact(&mut byte).await?;
        request.push(byte[0]);
    }
    assert_eq!(
        String::from_utf8(request)?.lines().next(),
        Some("GET /settings/v1?token=SYNTHETIC-PAIR HTTP/1.1")
    );
    let body = r#"{"version":1,"link_status":"link_required"}"#;
    socket
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await?;
    Ok(())
}

async fn screen_prompt(listener: &tokio::net::TcpListener, portos: bool, expired: bool) -> Result {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut socket, _) = listener.accept().await?;
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        socket.read_exact(&mut byte).await?;
        request.push(byte[0]);
    }
    assert_eq!(
        String::from_utf8(request)?.lines().next(),
        Some("GET /events?token=SYNTHETIC-PAIR HTTP/1.1")
    );
    let state = if portos {
        serde_json::json!({"state":"link_code", "user_code":"TEST-LINK", "verification_uri":"https://auth.portoperatingsystem.com/device", "expires_at_unix":if expired { 1 } else { u64::MAX }}) // lint-rules: allow-literal inert public prompt fixture
    } else {
        serde_json::json!({"state":"ready", "message":"code: TEST-CODE\nurl: https://auth.openai.com/codex/device\n"}) // lint-rules: allow-literal inert public prompt fixture
    };
    let body = format!("data: {{\"seq\":1,\"state\":{state}}}\n\n");
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n", body.len()).as_bytes()).await?;
    let mut byte = [0];
    let result = socket.read(&mut byte).await;
    assert!(matches!(result, Ok(0)) || result.is_err());
    Ok(())
}

fn assert_public(text: &str) {
    for marker in [
        "SYNTHETIC-DEVICE",
        "SYNTHETIC-PAIR",
        "SYNTHETIC-TOKEN",
        "SYNTHETIC-ACCOUNT",
        "SYNTHETIC-PRIVATE-PATH",
    ] {
        assert!(!text.contains(marker), "private boundary data escaped");
    }
}

pub(super) async fn start_prompted(
    fixture: &Fixture,
    json: bool,
) -> Result<(
    tokio::process::Child,
    std::sync::Arc<tokio::net::TcpListener>,
    tokio::net::TcpListener,
)> {
    let screen = std::sync::Arc::new(tokio::net::TcpListener::bind("127.0.0.1:0").await?);
    let health = fixture.1.as_ref().ok_or("missing service")?.address.port();
    let replies: Vec<_> = (0..8)
        .flat_map(|_| {
            [
                screen.local_addr().map(|addr| addr.port().to_string()),
                Ok(health.to_string()),
            ]
        })
        .collect::<io::Result<_>>()?;
    fs::write(fixture.0.join("forward-replies.json"), serde_json::to_vec(&replies)?)?;
    fs::write(fixture.0.join("setup-pairing"), "SYNTHETIC-PAIR\n")?;
    fs::write(
        fixture.0.join("confirm-evidence"),
        "wake=pending\nlink=pending\nrun=pending\n",
    )?;
    let barrier = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    fs::write(
        fixture.0.join("account-barrier"),
        barrier.local_addr()?.port().to_string(),
    )?;
    let mut command = tokio::process::Command::from(fixture.install());
    if !json {
        command = tokio::process::Command::from(fixture.command());
        command.args(["install", "--adb", env!("CARGO_BIN_EXE_echo-setup-test-adb")]);
    }
    let child = command
        .arg("--from-dir")
        .arg(fixture.bundle()?)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    Ok((child, screen, barrier))
}

async fn prompted_cli(json: bool) -> Result {
    use tokio::io::AsyncBufReadExt;
    let fixture = Fixture::new()?;
    let (mut child, screen, barrier) = start_prompted(&fixture, json).await?;
    let mut lines = tokio::io::BufReader::new(child.stdout.take().ok_or("missing stdout")?).lines();
    both_prompts(&mut lines, &screen, &barrier, json).await?;
    let instruction = lines.next_line().await?.ok_or("missing instruction")?;
    assert_public(&instruction);
    assert!(instruction.contains("Say Computer near the Echo."));
    fs::write(
        fixture.0.join("confirm-evidence"),
        "wake=confirmed\nlink=connected\nrun=started\n",
    )?;
    let result = lines.next_line().await?.ok_or("missing result")?;
    assert_public(&result);
    if json {
        assert_eq!(serde_json::from_str::<serde_json::Value>(&result)?["setup"], "complete");
    } else {
        assert_eq!(
            result,
            "Echo 0.2.0 installed, linked and confirmed a fresh wake and bridge run."
        );
    }
    assert_eq!(lines.next_line().await?, None);
    let output = child.wait_with_output().await?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stderr, b"");
    let remaining: Vec<String> = serde_json::from_slice(&fs::read(fixture.0.join("forwards.json"))?)?;
    assert_eq!(remaining, Vec::<String>::new());
    Ok(())
}

pub(super) async fn both_prompts(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    screen: &std::sync::Arc<tokio::net::TcpListener>,
    barrier: &tokio::net::TcpListener,
    json: bool,
) -> Result {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for portos in [false, true] {
        if portos {
            settings_reply(screen).await?;
        }
        let (mut account, _) = barrier.accept().await?;
        let mut ready = [0; 5];
        account.read_exact(&mut ready).await?;
        assert_eq!(&ready, b"ready");
        let listener = screen.clone();
        let server = tokio::task::spawn_local(async move {
            screen_prompt(&listener, portos, false)
                .await
                .map_err(|error| error.to_string())
        });
        let line = lines.next_line().await?.ok_or("missing prompt")?;
        check_prompt(&line, json, portos)?;
        account.write_all(&[1]).await?;
        server.await?.map_err(io::Error::other)?;
    }
    Ok(())
}

fn check_prompt(line: &str, json: bool, portos: bool) -> Result {
    assert_public(line);
    let (provider, code, url) = if portos {
        ("portos", "TEST-LINK", "https://auth.portoperatingsystem.com/device") // lint-rules: allow-literal inert public prompt expectation
    } else {
        ("chatgpt", "TEST-CODE", "https://auth.openai.com/codex/device") // lint-rules: allow-literal inert public prompt expectation
    };
    if json {
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(line)?,
            serde_json::json!({"type":"prompt", "provider":provider, "user_code":code, "verification_uri":url})
        );
    } else {
        assert_eq!(line, format!("{provider}: visit {url} and enter {code}."));
    }
    Ok(())
}

#[tokio::test]
async fn t3_cli_prompts_match_screen_and_completion_requires_fresh_run_in_text_and_json() -> Result {
    tokio::task::LocalSet::new()
        .run_until(async {
            for json in [false, true] {
                tokio::time::timeout(std::time::Duration::from_secs(10), prompted_cli(json)).await??;
            }
            Ok(())
        })
        .await
}

#[test]
fn t3_cli_unrepresentable_setup_deadline_returns_exit_three_and_cleans_only_stage() -> Result {
    let fixture = Fixture::new()?;
    let output = fixture
        .install()
        .arg("--from-dir")
        .arg(fixture.bundle()?)
        .args(["--timeout-seconds", "18446744073709551615"])
        .output()?;
    assert_eq!(output.status.code(), Some(3));
    let result = private_output(&output)?;
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"], "setup_timeout");
    assert!(result["setup"].is_null());
    let calls = fixture.ledger()?;
    assert!(
        calls
            .last()
            .and_then(|args| args.last())
            .is_some_and(|call| call.starts_with("rm -rf '/data/local/tmp/echo-setup-"))
    );
    assert!(
        !calls
            .iter()
            .filter_map(|args| args.last())
            .any(|call| call.contains("--device ensure-signin") || call.contains("--device uninstall"))
    );
    let remaining: Vec<String> = serde_json::from_slice(&fs::read(fixture.0.join("forwards.json"))?)?;
    assert_eq!(remaining, Vec::<String>::new());
    Ok(())
}

#[test]
fn t3_linked_update_cli_preserves_grant_and_completes_after_fresh_confirmation() -> Result {
    let fixture = Fixture::new()?;
    fs::write(fixture.0.join("link-state"), "linked")?;
    let output = fixture
        .command()
        .args([
            "update",
            "--json",
            "--adb",
            env!("CARGO_BIN_EXE_echo-setup-test-adb"),
            "--from-dir",
        ])
        .arg(fixture.bundle()?)
        .output()?;
    let result = private_output(&output)?;
    assert_eq!(output.status.code(), Some(0), "{result}");
    assert_eq!(
        private_output(&output)?,
        serde_json::json!({"type":"result", "command":"update",
        "ok":true, "version":"0.2.0", "setup":"complete"})
    );
    let calls = fixture.ledger()?;
    let operations: Vec<_> = calls.iter().filter_map(|args| args.last()).collect();
    assert!(
        !operations
            .iter()
            .any(|call| call.ends_with("--device link") || call.ends_with("--device signin"))
    );
    assert_eq!(
        operations
            .iter()
            .filter(|call| call.ends_with("--device ensure-signin"))
            .count(),
        1
    );
    assert!(operations.iter().any(|call| call.contains("--device confirm-check '")));
    let remaining: Vec<String> = serde_json::from_slice(&fs::read(fixture.0.join("forwards.json"))?)?;
    assert_eq!(remaining, Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn t3_expired_link_cli_reports_exit_three_guidance_and_drained_cleanup() -> Result {
    tokio::task::LocalSet::new()
        .run_until(async {
            for json in [false, true] {
                tokio::time::timeout(std::time::Duration::from_secs(10), expired_cli(json)).await??;
            }
            Ok(())
        })
        .await
}

async fn expired_cli(json: bool) -> Result {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let fixture = Fixture::new()?;
    let (mut child, screen, barrier) = start_prompted(&fixture, json).await?;
    let mut lines = tokio::io::BufReader::new(child.stdout.take().ok_or("missing stdout")?).lines();
    finish_signin(&mut lines, &screen, &barrier, json).await?;
    settings_reply(&screen).await?;
    let (mut link, _) = barrier.accept().await?;
    let mut ready = [0; 5];
    link.read_exact(&mut ready).await?;
    assert_eq!(&ready, b"ready");
    screen_prompt(&screen, true, true).await?;
    let result = lines.next_line().await?;
    expired_result(result.as_deref(), json)?;
    assert_eq!(lines.next_line().await?, None);
    let output = child.wait_with_output().await?;
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8(output.stderr)?;
    assert_public(&stderr);
    assert_eq!(stderr.contains("fresh Port OS code"), !json);
    expired_cleanup(&fixture)
}

async fn finish_signin(
    lines: &mut CliLines,
    screen: &std::sync::Arc<tokio::net::TcpListener>,
    barrier: &tokio::net::TcpListener,
    json: bool,
) -> Result {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = screen.clone();
    let server = tokio::task::spawn_local(async move {
        screen_prompt(&listener, false, false)
            .await
            .map_err(|error| error.to_string())
    });
    let (mut account, _) = barrier.accept().await?;
    let mut ready = [0; 5];
    account.read_exact(&mut ready).await?;
    assert_eq!(&ready, b"ready");
    check_prompt(&lines.next_line().await?.ok_or("missing signin prompt")?, json, false)?;
    account.write_all(&[1]).await?;
    server.await?.map_err(io::Error::other)?;
    Ok(())
}

fn expired_result(result: Option<&str>, json: bool) -> Result {
    if json {
        let value: serde_json::Value = serde_json::from_str(result.ok_or("missing failure")?)?;
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"], "setup_portos_code_expired");
        assert!(value["setup"].is_null());
        assert!(
            value["fix"]
                .as_str()
                .is_some_and(|fix| fix.contains("fresh Port OS code"))
        );
        assert_public(&value.to_string());
    } else {
        assert_eq!(result, None);
    }
    Ok(())
}

fn expired_cleanup(fixture: &Fixture) -> Result {
    failure_cleanup(fixture)?;
    assert!(fixture.0.join("process-user").exists());
    Ok(())
}

fn failure_cleanup(fixture: &Fixture) -> Result {
    let calls = fixture.ledger()?;
    let operations: Vec<_> = calls.iter().filter_map(|args| args.last()).collect();
    assert_eq!(
        operations.iter().filter(|call| call.ends_with("--device stop")).count(),
        3
    );
    assert!(
        !operations
            .iter()
            .any(|call| call.contains("--device uninstall") || call.contains("confirm-cursor"))
    );
    assert!(
        operations
            .last()
            .is_some_and(|call| call.starts_with("rm -rf '/data/local/tmp/echo-setup-"))
    );
    let remaining: Vec<String> = serde_json::from_slice(&fs::read(fixture.0.join("forwards.json"))?)?;
    assert_eq!(remaining, Vec::<String>::new());
    Ok(())
}
