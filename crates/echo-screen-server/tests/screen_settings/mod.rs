//! T3 real HTTP; the host is a controlled transaction observer, not a file store.
use super::support::{Result, TOKEN, request};
use echo_contracts::secret::Secret;
use echo_screen_contract::settings_v1::{SaveResultV1, SettingsVersionV1};
use echo_screen_server::{
    ScreenServer, SettingsOperation, SettingsReply, SettingsRequest, serve_with_settings, settings_channel,
};
use tokio::sync::mpsc;

const PATCH: &str = r#"{"version":1,"settings":{"agent":{"max_tool_calls":8}}}"#;
const JSON: &str = "Content-Type: application/json\r\n";

async fn mounted() -> Result<(
    ScreenServer,
    mpsc::Sender<echo_screen_contract::UiState>,
    mpsc::Receiver<SettingsRequest>,
)> {
    let (host, inbox) = settings_channel();
    let (states, input) = mpsc::channel(32);
    let server = serve_with_settings("127.0.0.1:0".parse()?, Secret::new(TOKEN.into()), input, Some(host)).await?;
    Ok((server, states, inbox))
}

fn saved() -> SettingsReply {
    SettingsReply::Saved(SaveResultV1 {
        version: SettingsVersionV1,
        restart_pending: false,
    })
}

#[tokio::test]
async fn t3_settings_guards_refuse_without_host_admission() -> Result {
    let (mut server, _states, mut inbox) = mounted().await?;
    let addr = server.bound_addr;
    let path = format!("/settings/v1?token={TOKEN}");
    for (target, body, headers, code) in [
        ("/settings/v1", "not json", "", 401),
        ("/settings/v1?token=", PATCH, JSON, 401),
        ("/settings/v1?token=wrong", PATCH, JSON, 401),
        (&format!("{path}&token={TOKEN}"), PATCH, JSON, 401),
        (&path, PATCH, "Origin: http://127.0.0.1:1\r\n", 403),
        (&path, PATCH, "", 415),
        (&path, "not json", JSON, 400),
        (&path, r#"{"version":1,"settings":{}}"#, JSON, 400),
        (&path, r#"{"version":1,"settings":{"voice":{}}}"#, JSON, 400),
        (
            &path,
            r#"{"version":1,"settings":{"agent":{"max_tool_calls":8,"extra":0}}}"#,
            JSON,
            400,
        ),
        (
            &path,
            r#"{"version":2,"settings":{"agent":{"max_tool_calls":8}}}"#,
            JSON,
            400,
        ),
        (&format!("/settings/v1/prompts/other?token={TOKEN}"), "{}", JSON, 404),
    ] {
        assert_eq!(
            request(addr, "POST", target, body, headers).await?,
            (code, String::new())
        );
        assert!(matches!(inbox.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
    }
    assert_eq!(request(addr, "POST", &path, &" ".repeat(65_537), JSON).await?.0, 413);
    let foreign = "Origin: http://127.0.0.1:1\r\n";
    assert_eq!(request(addr, "GET", &path, "", foreign).await?, (403, String::new()));
    assert!(matches!(inbox.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn t3_settings_typed_transaction_and_prompt_encoded_limit() -> Result {
    let (mut server, _states, mut inbox) = mounted().await?;
    let addr = server.bound_addr;
    let host = tokio::spawn(async move {
        inbox
            .recv()
            .await
            .unwrap()
            .commit(|operation| {
                let SettingsOperation::Save(patch) = operation else {
                    unreachable!()
                };
                assert_eq!(patch.settings.agent.unwrap().max_tool_calls, Some(8));
                saved()
            })
            .await;
        inbox
            .recv()
            .await
            .unwrap()
            .commit(|operation| {
                let SettingsOperation::Prompt(echo_screen_server::PromptKind::Voice, prompt) = operation else {
                    unreachable!()
                };
                assert_eq!(prompt.text.as_bytes(), vec![b'a'; 32_768]);
                saved()
            })
            .await;
        assert!(matches!(inbox.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
    });
    let path = format!("/settings/v1?token={TOKEN}");
    let headers = format!("{JSON}Origin: http://{addr}\r\n");
    assert_eq!(
        request(addr, "POST", &path, PATCH, &headers).await?,
        (200, r#"{"version":1,"restart_pending":false}"#.into())
    );
    let path = format!("/settings/v1/prompts/voice?token={TOKEN}");
    let body = format!(r#"{{"version":1,"text":"{}"}}"#, "\\u0061".repeat(32_768));
    assert_eq!(request(addr, "POST", &path, &body, JSON).await?.0, 200);
    assert_eq!(request(addr, "POST", &path, &" ".repeat(196_673), JSON).await?.0, 413);
    host.await?;
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn t3_settings_revocation_between_dispatch_and_commit_invokes_no_transaction() -> Result {
    let (mut server, _states, mut inbox) = mounted().await?;
    let addr = server.bound_addr;
    let pending =
        tokio::spawn(async move { request(addr, "POST", &format!("/settings/v1?token={TOKEN}"), PATCH, JSON).await });
    let admitted = inbox.recv().await.ok_or("missing host request")?;
    server.revoke_pairing().await?;
    admitted.commit(|_| unreachable!("revoked request executed")).await;
    assert_eq!(pending.await??, (401, String::new()));
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn t3_settings_snapshot_and_safe_host_errors_have_exact_wire() -> Result {
    use echo_screen_contract::settings_v1::{SettingsErrorV1, SettingsSnapshotV1};
    const SNAPSHOT: &str = r#"{"version":1,"settings":{"voice":{"session_listen_enabled":true,"session_listen_seconds":15},"codex":{"voice":"default"},"agent":{"daily_request_cap":"9223372036854775807","max_tool_calls":8}},"allowed_voices":["default"],"prompts":{"voice":"voice text","runner":"runner text"},"link_status":"unconfigured","bridge_version":"synthetic","restart_pending":false,"warnings":[]}"#;
    let (mut server, _states, mut inbox) = mounted().await?;
    let host = tokio::spawn(async move {
        inbox
            .recv()
            .await
            .unwrap()
            .commit(|operation| {
                assert!(matches!(operation, SettingsOperation::Snapshot));
                SettingsReply::Snapshot(serde_json::from_str::<SettingsSnapshotV1>(SNAPSHOT).unwrap())
            })
            .await;
        for persistence in [false, true] {
            inbox
                .recv()
                .await
                .unwrap()
                .commit(|operation| {
                    assert!(matches!(operation, SettingsOperation::Save(_)));
                    let error = SettingsErrorV1 {
                        version: SettingsVersionV1,
                        message: "fixed safe error".into(),
                    };
                    if persistence {
                        SettingsReply::Persistence(error)
                    } else {
                        SettingsReply::Invalid(error)
                    }
                })
                .await;
        }
    });
    let path = format!("/settings/v1?token={TOKEN}");
    assert_eq!(
        request(server.bound_addr, "GET", &path, "", "").await?,
        (200, SNAPSHOT.into())
    );
    for code in [422, 500] {
        assert_eq!(
            request(server.bound_addr, "POST", &path, PATCH, JSON).await?,
            (code, r#"{"version":1,"message":"fixed safe error"}"#.into())
        );
    }
    host.await?;
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn t3_settings_rechecks_pairing_after_a_held_body() -> Result {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let (mut server, _states, mut inbox) = mounted().await?;
    let mut socket = tokio::net::TcpStream::connect(server.bound_addr).await?;
    socket.write_all(format!("POST /settings/v1?token={TOKEN} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{JSON}Content-Length: {}\r\nExpect: 100-continue\r\n\r\n", PATCH.len()).as_bytes()).await?;
    let mut socket = BufReader::new(socket);
    let mut line = String::new();
    socket.read_line(&mut line).await?;
    assert_eq!(line, "HTTP/1.1 100 Continue\r\n");
    line.clear();
    socket.read_line(&mut line).await?;
    assert_eq!(line, "\r\n");
    server.revoke_pairing().await?;
    socket.get_mut().write_all(PATCH.as_bytes()).await?;
    let mut tail = String::new();
    socket.read_to_string(&mut tail).await?;
    assert!(tail.starts_with("HTTP/1.1 401 Unauthorized\r\n"));
    assert!(matches!(inbox.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn t3_settings_absent_closed_host_and_capability_are_explicit() -> Result {
    let (mut baseline, _states) = super::support::server().await?;
    let original = request(baseline.bound_addr, "GET", "/", "", "").await?;
    assert_eq!(original.0, 200);
    assert_eq!(original.1, include_str!("../../../../web/screen/src/index.html"));
    for method in ["GET", "POST"] {
        assert_eq!(
            request(
                baseline.bound_addr,
                method,
                &format!("/settings/v1?token={TOKEN}"),
                PATCH,
                JSON
            )
            .await?,
            (503, String::new())
        );
    }
    baseline.shutdown().await?;
    let (mut server, _states, inbox) = mounted().await?;
    let enabled = request(server.bound_addr, "GET", "/", "", "").await?;
    assert_eq!(
        enabled.1,
        original.1.replacen(
            "<head>",
            "<head>\n    <meta name=\"echo-settings\" content=\"v1\" />",
            1
        )
    );
    drop(inbox);
    assert_eq!(
        request(
            server.bound_addr,
            "POST",
            &format!("/settings/v1?token={TOKEN}"),
            PATCH,
            JSON
        )
        .await?,
        (503, String::new())
    );
    server.shutdown().await?;
    Ok(())
}
