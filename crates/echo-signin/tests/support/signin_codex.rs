//! Controlled account peer at the actual app-server JSONL boundary.
use echo_codex::{codex_io::RpcIo, codex_rpc::Limits};
use echo_config::LiveMode;
use echo_contracts::secret::Secret;
use echo_signin::{
    OperatorCommand,
    signin_codex::{AccountError, authenticate, execute},
};
use serde_json::{Value, json};
use std::{io::Write, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf},
    time::Instant,
};

type Client = RpcIo<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

fn pair() -> (Client, BufReader<DuplexStream>) {
    let (client, peer) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(client);
    (
        RpcIo::new(read, write, Limits::default()).unwrap(),
        BufReader::new(peer),
    )
}

async fn request(peer: &mut BufReader<DuplexStream>, method: &str, params: Value) -> Value {
    let mut line = String::new();
    peer.read_line(&mut line).await.unwrap();
    let received: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(received["method"], method);
    assert_eq!(received["params"], params);
    assert!(received.get("jsonrpc").is_none());
    received["id"].clone()
}

async fn send(peer: &mut BufReader<DuplexStream>, value: Value) {
    peer.get_mut().write_all(format!("{value}\n").as_bytes()).await.unwrap();
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

fn prompt(browser: bool) -> Value {
    if browser {
        json!({"type":"chatgpt", "loginId":"login-current", "authUrl":"http://127.0.0.1:12345/verify"})
    } else {
        json!({"type":"chatgptDeviceCode", "loginId":"login-current", "verificationUrl":"http://127.0.0.1:12345/verify", "userCode":"TEST-CODE"})
    }
}

fn completed(login_id: Value, success: Value) -> Value {
    let mut notification =
        json!({"method":"account/login/completed", "params":{"error":"synthetic-private-provider-detail"}});
    notification["params"]["loginId"] = login_id;
    notification["params"]["success"] = success;
    notification
}

#[tokio::test(start_paused = true)]
async fn codex_signin_requires_matching_completion_and_presents_identical_prompt() {
    for browser in [false, true] {
        let (mut io, mut peer) = pair();
        let mut writer = Vec::new();
        let mut screen = String::new();
        let expected = if browser {
            "url: http://127.0.0.1:12345/verify\n"
        } else {
            "code: TEST-CODE\nurl: http://127.0.0.1:12345/verify\n"
        };
        let operation = execute(
            &mut io,
            LiveMode::Chatgpt,
            OperatorCommand::Signin { browser },
            &mut writer,
            |prompt| {
                screen = prompt.message();
                Ok(())
            },
            deadline(),
        );
        let peer_work = async {
            let kind = if browser { "chatgpt" } else { "chatgptDeviceCode" };
            let id = request(&mut peer, "account/login/start", json!({"type":kind})).await;
            send(&mut peer, json!({"id":id,"result":prompt(browser)})).await;
            send(&mut peer, completed(json!("login-old"), json!(false))).await;
            send(&mut peer, completed(Value::Null, json!(true))).await;
            send(
                &mut peer,
                json!({"method":"account/updated","params":{"authMode":"chatgpt"}}),
            )
            .await;
            send(&mut peer, completed(json!("login-current"), json!(true))).await;
        };
        let (result, ()) = tokio::join!(operation, peer_work);
        assert_eq!(result, Ok(()));
        assert_eq!(screen, expected);
        assert_eq!(writer, format!("{expected}signed in\n").as_bytes());
    }
}

#[tokio::test(start_paused = true)]
async fn codex_signin_mismatch_does_not_succeed_and_deadline_is_absolute() {
    let (mut io, mut peer) = pair();
    let mut writer = Vec::new();
    let operation = execute(
        &mut io,
        LiveMode::Chatgpt,
        OperatorCommand::Signin { browser: false },
        &mut writer,
        |_| Ok(()),
        deadline(),
    );
    let peer_work = async {
        let id = request(&mut peer, "account/login/start", json!({"type":"chatgptDeviceCode"})).await;
        send(&mut peer, json!({"id":id,"result":prompt(false)})).await;
        send(&mut peer, completed(json!("login-old"), json!(true))).await;
        // Tokio's paused clock advances directly to the absolute completion deadline.
    };
    let (result, ()) = tokio::join!(operation, peer_work);
    assert_eq!(result, Err(AccountError::Deadline));
    assert_eq!(writer, b"code: TEST-CODE\nurl: http://127.0.0.1:12345/verify\n");
    assert!(io.begin_request("account/read", &json!({})).is_err());
}

#[tokio::test(start_paused = true)]
async fn codex_signin_failure_and_malformed_completion_are_sanitized() {
    for (reply, expected) in [
        (
            completed(json!("login-current"), json!(false)),
            AccountError::Authorization,
        ),
        (
            completed(json!("login-current"), Value::Null),
            AccountError::InvalidResponse,
        ),
        (json!({"method":"account/updated","params":{}}), AccountError::Deadline),
    ] {
        let (mut io, mut peer) = pair();
        let mut writer = Vec::new();
        let operation = execute(
            &mut io,
            LiveMode::Chatgpt,
            OperatorCommand::Signin { browser: false },
            &mut writer,
            |_| Ok(()),
            deadline(),
        );
        let peer_work = async {
            let id = request(&mut peer, "account/login/start", json!({"type":"chatgptDeviceCode"})).await;
            send(&mut peer, json!({"id":id,"result":prompt(false)})).await;
            send(&mut peer, reply).await;
        };
        let (result, ()) = tokio::join!(operation, peer_work);
        assert_eq!(result, Err(expected));
        assert!(!String::from_utf8_lossy(&writer).contains("signed in"));
        assert!(!String::from_utf8_lossy(&writer).contains("synthetic-private"));
        assert!(!expected.to_string().contains("synthetic-private"));
    }
}

#[tokio::test(start_paused = true)]
async fn codex_signin_rejects_invalid_prompt_before_any_presentation() {
    for mutation in [
        json!({"userCode":"injected\ntext"}),
        json!({"loginId":""}),
        json!({"verificationUrl":"javascript:alert(1)"}),
        json!({"verificationUrl":"http://example.invalid/verify"}), // lint-rules: allow-literal inert rejected prompt; never dialed
        json!({"verificationUrl":"https://user:password@example.invalid/verify"}), // lint-rules: allow-literal inert rejected prompt; never dialed
        json!({"type":"apiKey"}),
    ] {
        let (mut io, mut peer) = pair();
        let mut writer = Vec::new();
        let mut presented = false;
        let operation = execute(
            &mut io,
            LiveMode::Chatgpt,
            OperatorCommand::Signin { browser: false },
            &mut writer,
            |_| {
                presented = true;
                Ok(())
            },
            deadline(),
        );
        let peer_work = async {
            let id = request(&mut peer, "account/login/start", json!({"type":"chatgptDeviceCode"})).await;
            let mut result = prompt(false);
            result
                .as_object_mut()
                .unwrap()
                .extend(mutation.as_object().unwrap().clone());
            send(&mut peer, json!({"id":id,"result":result})).await;
        };
        let (result, ()) = tokio::join!(operation, peer_work);
        assert_eq!(result, Err(AccountError::InvalidResponse));
        assert_eq!(writer, b"");
        assert!(!presented);
    }
}

#[tokio::test(start_paused = true)]
async fn codex_signin_output_and_screen_failure_close_generation_without_success() {
    for output_fails in [false, true] {
        let (mut io, mut peer) = pair();
        let mut bytes = [0; 1];
        let mut cursor = std::io::Cursor::new(bytes.as_mut_slice());
        let mut output = Vec::new();
        let writer: &mut dyn Write = if output_fails { &mut cursor } else { &mut output };
        let mut writer = writer;
        let operation = execute(
            &mut io,
            LiveMode::Chatgpt,
            OperatorCommand::Signin { browser: false },
            &mut writer,
            |_| Err(AccountError::Output),
            deadline(),
        );
        let peer_work = async {
            let id = request(&mut peer, "account/login/start", json!({"type":"chatgptDeviceCode"})).await;
            send(&mut peer, json!({"id":id,"result":prompt(false)})).await;
        };
        let (result, ()) = tokio::join!(operation, peer_work);
        assert_eq!(result, Err(AccountError::Output));
        assert!(!String::from_utf8_lossy(&output).contains("signed in"));
        assert!(io.begin_request("account/read", &json!({})).is_err());
    }
}

#[tokio::test(start_paused = true)]
async fn codex_api_key_is_nominated_and_confirmed_by_account_read() {
    let (mut io, mut peer) = pair();
    let key = Secret::new("dummy-local-only".into());
    let operation = authenticate(&mut io, LiveMode::ApiKey, Some(&key));
    let peer_work = async {
        let id = request(
            &mut peer,
            "account/login/start",
            json!({"type":"apiKey","apiKey":"dummy-local-only"}),
        )
        .await;
        send(&mut peer, json!({"id":id,"result":{"type":"apiKey"}})).await;
        let id = request(&mut peer, "account/read", json!({"refreshToken":false})).await;
        send(
            &mut peer,
            json!({"id":id,"result":{"account":{"type":"apiKey"},"requiresOpenaiAuth":true}}),
        )
        .await;
    };
    let (result, ()) = tokio::join!(operation, peer_work);
    assert_eq!(result, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn codex_status_masks_metadata_and_does_not_fabricate_expiry() {
    let (mut io, mut peer) = pair();
    let mut writer = Vec::new();
    let operation = execute(
        &mut io,
        LiveMode::Chatgpt,
        OperatorCommand::Status,
        &mut writer,
        |_| Ok(()),
        deadline(),
    );
    let peer_work = async {
        let id = request(&mut peer, "account/read", json!({"refreshToken":false})).await;
        send(&mut peer, json!({"id":id,"result":{"account":{"type":"chatgpt","email":"synthetic@example.invalid","planType":"test"},"requiresOpenaiAuth":true}})).await;
    };
    let (result, ()) = tokio::join!(operation, peer_work);
    assert_eq!(result, Ok(()));
    assert_eq!(writer, b"mode: chatgpt\naccount: ****\nexpires: n/a\n");
}

#[tokio::test(start_paused = true)]
async fn codex_account_transport_remote_and_malformed_replies_are_safe() {
    for (response, expected) in [
        (
            Some(json!({"error":{"code":-32000,"message":"synthetic-private-detail"}})),
            AccountError::Authorization,
        ),
        (
            Some(json!({"result":{"account":"malformed"}})),
            AccountError::InvalidResponse,
        ),
        (None, AccountError::Transport),
    ] {
        let (mut io, mut peer) = pair();
        let mut writer = Vec::new();
        let operation = execute(
            &mut io,
            LiveMode::Chatgpt,
            OperatorCommand::Status,
            &mut writer,
            |_| Ok(()),
            deadline(),
        );
        let peer_work = async {
            let id = request(&mut peer, "account/read", json!({"refreshToken":false})).await;
            if let Some(mut response) = response {
                response["id"] = id;
                send(&mut peer, response).await;
            } else {
                peer.get_mut().shutdown().await.unwrap();
            }
        };
        let (result, ()) = tokio::join!(operation, peer_work);
        assert_eq!(result, Err(expected));
        assert_eq!(writer, b"");
        assert!(!expected.to_string().contains("synthetic-private"));
    }
}

#[tokio::test(start_paused = true)]
async fn codex_status_signed_out_wrong_mode_api_key_and_output_failure() {
    for (mode, account, broken_output, expected) in [
        (LiveMode::Chatgpt, Value::Null, false, Err(AccountError::Authorization)),
        (
            LiveMode::Chatgpt,
            json!({"type":"apiKey"}),
            false,
            Err(AccountError::InvalidResponse),
        ),
        (
            LiveMode::ApiKey,
            json!({"type":"chatgpt","planType":"test"}),
            false,
            Err(AccountError::InvalidResponse),
        ),
        (LiveMode::ApiKey, Value::Null, false, Ok(())),
        (
            LiveMode::ApiKey,
            json!({"type":"apiKey"}),
            true,
            Err(AccountError::Output),
        ),
    ] {
        let (mut io, mut peer) = pair();
        let mut bytes = [0; 1];
        let mut cursor = std::io::Cursor::new(bytes.as_mut_slice());
        let mut output = Vec::new();
        let mut writer: &mut dyn Write = if broken_output { &mut cursor } else { &mut output };
        let operation = execute(
            &mut io,
            mode,
            OperatorCommand::Status,
            &mut writer,
            |_| Ok(()),
            deadline(),
        );
        let peer_work = async {
            let id = request(&mut peer, "account/read", json!({"refreshToken":false})).await;
            send(
                &mut peer,
                json!({"id":id,"result":{"account":account,"requiresOpenaiAuth":true}}),
            )
            .await;
        };
        let (result, ()) = tokio::join!(operation, peer_work);
        assert_eq!(result, expected);
        if expected.is_ok() {
            assert_eq!(output, b"mode: api-key\naccount: n/a\nexpires: n/a\n");
        } else {
            assert_eq!(output, b"");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn codex_authenticate_rejects_missing_key_wrong_mode_and_signed_out_account() {
    let (mut io, _peer) = pair();
    assert_eq!(
        authenticate(&mut io, LiveMode::ApiKey, None).await,
        Err(AccountError::Authorization)
    );
    for account in [
        Value::Null,
        json!({"type":"apiKey"}),
        json!({"type":"chatgpt","planType":"test"}),
    ] {
        let (mut io, mut peer) = pair();
        let operation = authenticate(&mut io, LiveMode::Chatgpt, None);
        let signed_in = account["type"] == "chatgpt";
        let peer_work = async {
            let id = request(&mut peer, "account/read", json!({"refreshToken":false})).await;
            send(
                &mut peer,
                json!({"id":id,"result":{"account":account,"requiresOpenaiAuth":true}}),
            )
            .await;
        };
        let (result, ()) = tokio::join!(operation, peer_work);
        assert_eq!(
            result,
            if signed_in {
                Ok(())
            } else {
                Err(AccountError::Authorization)
            }
        );
    }
    let (mut io, mut peer) = pair();
    let key = Secret::new("dummy-local-only".into());
    let operation = authenticate(&mut io, LiveMode::ApiKey, Some(&key));
    let peer_work = async {
        let id = request(
            &mut peer,
            "account/login/start",
            json!({"type":"apiKey","apiKey":"dummy-local-only"}),
        )
        .await;
        send(&mut peer, json!({"id":id,"result":{"type":"chatgptDeviceCode"}})).await;
    };
    let (result, ()) = tokio::join!(operation, peer_work);
    assert_eq!(result, Err(AccountError::InvalidResponse));
}

#[tokio::test(start_paused = true)]
async fn codex_unsupported_commands_have_zero_wire_effects() {
    for (command, expected) in [
        (
            OperatorCommand::ImportCodex {
                path: "unused-source".into(),
            },
            AccountError::ImportUnsupported,
        ),
        (
            OperatorCommand::Signin { browser: false },
            AccountError::UnsupportedMode,
        ),
        (
            OperatorCommand::KillSwitch { enabled: true },
            AccountError::UnsupportedMode,
        ),
    ] {
        let (mut io, mut peer) = pair();
        let mut writer = Vec::new();
        assert_eq!(
            execute(&mut io, LiveMode::ApiKey, command, &mut writer, |_| Ok(()), deadline()).await,
            Err(expected)
        );
        let mut received = String::new();
        assert_eq!(peer.read_line(&mut received).await.unwrap(), 0);
        assert_eq!(writer, b"");
    }
}

#[tokio::test(start_paused = true)]
async fn startup_network_account_classification_uses_decoded_wire_without_exposing_details() {
    for (message, expected) in [
        ("workspace routing discovery failed", AccountError::Transport),
        ("workspace routing discovery timed out", AccountError::Transport),
        (
            "workspace routing discovery unauthorized (401)",
            AccountError::Authorization,
        ),
        ("synthetic-private-detail", AccountError::Authorization),
    ] {
        let (mut io, mut peer) = pair();
        let server = async {
            let id = request(&mut peer, "account/read", json!({"refreshToken":false})).await;
            send(
                &mut peer,
                json!({"id":id,"error":{"code":-32603,"message":message,
                "data":"synthetic-private-detail"}}),
            )
            .await;
        };
        let (result, ()) = tokio::join!(authenticate(&mut io, LiveMode::Chatgpt, None), server);
        assert_eq!(result, Err(expected));
        assert!(!expected.to_string().contains("private"));
    }
}
