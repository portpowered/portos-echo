//! T3 guided-account owner through real native processes and authenticated HTTP.
use super::*;
use echo_setup::setup::Prompt;
use std::io::ErrorKind;
use tokio::sync::oneshot;

fn prepare(fixture: &Fixture, screen: u16, verb: &str) -> Result {
    fs::write(
        fixture.0.join("account-reply"),
        format!("echo-install: {verb} completed\r\n"),
    )?;
    fs::write(fixture.0.join("stop-reply"), "echo-install: stopped\n")?;
    fs::write(fixture.0.join("setup-pairing"), "SYNTHETIC-PAIR\n")?;
    let replies = [screen.to_string(), "41002".to_owned()];
    fs::write(fixture.0.join("forward-replies.json"), serde_json::to_vec(&replies)?)?;
    fs::write(fixture.0.join("forwards.json"), "[]")?;
    fs::write(fixture.0.join("forward-targets.json"), "{}")?;
    Ok(())
}

fn cleaned(fixture: &Fixture, stopped: bool) -> Result {
    let remaining: Vec<String> = serde_json::from_slice(&fs::read(fixture.0.join("forwards.json"))?)?;
    assert_eq!(remaining, Vec::<String>::new());
    let calls = fixture.calls()?;
    assert_eq!(
        calls
            .iter()
            .filter(|args| args.get(3).is_some_and(|arg| arg == "sh -s -- --device stop"))
            .count(),
        usize::from(stopped)
    );
    assert_eq!(
        calls
            .iter()
            .filter(|args| args.get(2).is_some_and(|arg| arg == "forward")
                && args.get(3).is_some_and(|arg| arg == "--remove"))
            .count(),
        2
    );
    if stopped {
        let stop = calls
            .iter()
            .position(|args| args.get(3).is_some_and(|arg| arg == "sh -s -- --device stop"));
        let remove = calls
            .iter()
            .position(|args| args.get(3).is_some_and(|arg| arg == "--remove"));
        assert!(stop.is_some() && stop < remove);
    }
    Ok(())
}

fn frame(portos: bool) -> &'static str {
    if portos {
        "{\"state\":\"link_code\",\"user_code\":\"TEST-LINK\",\"verification_uri\":\"https://auth.portoperatingsystem.com/device\",\"expires_at_unix\":10000}" // lint-rules: allow-literal inert authenticated prompt fixture
    } else {
        "{\"state\":\"ready\",\"message\":\"code: TEST-CODE\\nurl: https://auth.openai.com/codex/device\\n\"}" // lint-rules: allow-literal inert authenticated prompt fixture
    }
}

async fn stream(listener: TcpListener, state: &'static str) -> Result {
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
    let body = format!("data: {{\"seq\":1,\"state\":{state}}}\n\ndata: {{\"seq\":2,\"state\":{state}}}\n\n");
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n", body.len()).as_bytes()).await?;
    let mut byte = [0];
    let read = socket
        .read(&mut byte)
        .await
        .map_err(|error| format!("SSE client close: {error:?}"))?;
    assert_eq!(read, 0, "SSE client must close after the response");
    Ok(())
}

fn presented(prompt: &Prompt, expected: &str) -> echo_setup::Result<()> {
    let value = serde_json::to_value(prompt).map_err(|_| Failure("output_failed"))?;
    assert_eq!(value["user_code"], expected);
    assert!(!value.to_string().contains("SYNTHETIC"));
    Ok(())
}

#[tokio::test]
async fn t3_guided_account_presents_each_provider_once_and_waits_for_command() -> Result {
    for portos in [false, true] {
        let fixture = Fixture::new()?;
        let connection = fixture.connection(deadline()).await?;
        let screen = TcpListener::bind("127.0.0.1:0").await?;
        let command = if portos {
            AccountCommand::Link
        } else {
            AccountCommand::EnsureSignin
        };
        prepare(
            &fixture,
            screen.local_addr()?.port(),
            if portos { "link" } else { "ensure-signin" },
        )?;
        let barrier = fixture.listener("account-barrier")?;
        let server = tokio::spawn(stream(screen, frame(portos)));
        let (sent, received) = oneshot::channel();
        let mut sent = Some(sent);
        let mut count = 0;
        let mut guided = Box::pin(connection.guide_account(command, 100, |prompt| {
            presented(prompt, if portos { "TEST-LINK" } else { "TEST-CODE" })?;
            count += 1;
            if let Some(sent) = sent.take() {
                let _ = sent.send(());
            }
            Ok(())
        }));
        let mut child = tokio::select! {
            result = &mut guided => return Err(format!("early account result: {result:?}").into()),
            socket = ready(&barrier) => socket?,
        };
        tokio::select! {
            result = &mut guided => return Err(format!("prompt completed command: {result:?}").into()),
            result = received => result?,
        }
        // Poll queued duplicate frames before allowing the command to finish.
        tokio::select! {
            biased;
            result = &mut guided => return Err(format!("unexpected result: {result:?}").into()),
            () = std::future::ready(()) => {}
        }
        child.write_all(&[1]).await?;
        guided.await.map_err(diagnostic)?;
        assert_eq!(count, 1);
        server.await??;
        cleaned(&fixture, false)?;
    }
    Ok(())
}

#[tokio::test]
async fn t3_guided_existing_account_needs_no_screen_and_ambiguous_completion_settles() -> Result {
    for fail in [false, true] {
        let fixture = Fixture::new()?;
        let connection = fixture.connection(deadline()).await?;
        prepare(&fixture, 41001, "ensure-signin")?;
        if fail {
            fs::write(fixture.0.join("account-reply"), "SYNTHETIC-ACCOUNT")?;
        }
        let mut count = 0;
        let result = connection
            .guide_account(AccountCommand::EnsureSignin, 100, |_| {
                count += 1;
                Ok(())
            })
            .await;
        assert_eq!(
            result,
            if fail {
                Err(Failure("setup_account_failed"))
            } else {
                Ok(())
            }
        );
        assert_eq!(count, 0);
        cleaned(&fixture, fail)?;
    }
    Ok(())
}

#[tokio::test]
async fn t3_guided_observation_and_output_failures_settle_before_returning() -> Result {
    for output_failure in [false, true] {
        let fixture = Fixture::new()?;
        let connection = fixture.connection(deadline()).await?;
        let screen = TcpListener::bind("127.0.0.1:0").await?;
        prepare(&fixture, screen.local_addr()?.port(), "signin")?;
        let barrier = fixture.listener("account-barrier")?;
        let state = if output_failure {
            frame(false)
        } else {
            "{\"state\":\"error\",\"relink\":false,\"message\":\"SYNTHETIC-ACCOUNT\"}"
        };
        let (publish, account_ready) = oneshot::channel();
        let server = tokio::spawn(async move {
            account_ready.await?;
            tokio::time::timeout(Duration::from_secs(5), stream(screen, state))
                .await
                .map_err(|_| "SSE exchange did not finish before the deadline")?
        });
        let mut count = 0;
        let mut guided = Box::pin(connection.guide_account(AccountCommand::Signin, 100, |_| {
            count += 1;
            Err(Failure("output_failed"))
        }));
        let mut child = tokio::select! {
            result = &mut guided => return Err(format!("guide completed before the account child was ready: {result:?}").into()),
            socket = ready(&barrier) => socket?,
        };
        // The screen cannot deliver a terminal frame before we observe the
        // account child blocked at its readiness barrier.
        publish.send(()).map_err(|()| "SSE peer dropped before publication")?;
        let result = guided.await;
        assert_eq!(
            result,
            Err(Failure(if output_failure {
                "output_failed"
            } else {
                "setup_provider_failed"
            }))
        );
        assert_eq!(count, usize::from(output_failure));
        cleaned(&fixture, true)?;
        let mut byte = [0];
        let closed = tokio::time::timeout(Duration::from_secs(5), child.read(&mut byte))
            .await
            .map_err(|_| "account child socket did not close before the deadline")?;
        // Forced child termination can reset the readiness connection on Windows.
        assert!(
            matches!(closed, Ok(0))
                || closed
                    .as_ref()
                    .is_err_and(|error| error.kind() == ErrorKind::ConnectionReset),
            "account child socket must close without sending another receipt: {closed:?}"
        );
        server.await??;
    }
    Ok(())
}

#[tokio::test]
async fn t3_guided_timeout_has_independent_settlement_and_forward_cleanup() -> Result {
    timed_out_account(false, 100, 11, "setup_chatgpt_timeout").await?;
    timed_out_account(true, 100, 11, "setup_portos_timeout").await
}

#[tokio::test]
async fn t3_presented_portos_code_expires_without_another_screen_frame() -> Result {
    timed_out_account(true, 9998, 2, "setup_portos_code_expired").await
}

async fn timed_out_account(portos: bool, now_unix: u64, advance: u64, expected: &'static str) -> Result {
    let fixture = Fixture::new()?;
    let connection = fixture.connection(deadline()).await?;
    let screen = TcpListener::bind("127.0.0.1:0").await?;
    let (command, verb) = if portos {
        (AccountCommand::Link, "link")
    } else {
        (AccountCommand::Signin, "signin")
    };
    prepare(&fixture, screen.local_addr()?.port(), verb)?;
    let barrier = fixture.listener("account-barrier")?;
    let server = tokio::spawn(stream(screen, frame(portos)));
    let (sent, received) = oneshot::channel();
    let mut sent = Some(sent);
    let mut guided = Box::pin(connection.guide_account(command, now_unix, |_| {
        if let Some(sent) = sent.take() {
            let _ = sent.send(());
        }
        Ok(())
    }));
    let child = tokio::select! {
        result = &mut guided => return Err(format!("early result: {result:?}").into()),
        socket = ready(&barrier) => socket?,
    };
    tokio::select! {
        result = &mut guided => return Err(format!("prompt completed command: {result:?}").into()),
        result = received => result?,
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(advance)).await;
    // Resume before subprocess cleanup: its OS work is observed through actual
    // pipe readiness, while only the setup deadline was advanced artificially.
    tokio::time::resume();
    assert_eq!(guided.await, Err(Failure(expected)));
    drop(child);
    server.await??;
    cleaned(&fixture, true)?;
    Ok(())
}

#[tokio::test]
async fn t3_guided_cleanup_refusals_take_priority_without_skipping_other_cleanup() -> Result {
    for stop_failure in [false, true] {
        let fixture = Fixture::new()?;
        let connection = fixture.connection(deadline()).await?;
        prepare(&fixture, 41001, "signin")?;
        fs::write(fixture.0.join("account-reply"), "SYNTHETIC-TOKEN")?;
        if stop_failure {
            fs::write(fixture.0.join("stop-reply"), "SYNTHETIC-ACCOUNT")?;
        } else {
            fs::write(fixture.0.join("remove-fails"), "")?;
        }
        let result = connection.guide_account(AccountCommand::Signin, 100, |_| Ok(())).await;
        assert_eq!(
            result,
            Err(Failure(if stop_failure {
                "setup_account_unsettled"
            } else {
                "setup_forward_cleanup_failed"
            }))
        );
        if stop_failure {
            cleaned(&fixture, true)?;
            let before = fixture.calls()?.len();
            assert_eq!(
                connection.account(AccountCommand::Link).await,
                Err(Failure("setup_account_unsettled"))
            );
            assert_eq!(fixture.calls()?.len(), before);
        } else {
            let calls = fixture.calls()?;
            assert_eq!(
                calls
                    .iter()
                    .filter(|args| args.get(3).is_some_and(|arg| arg == "--remove"))
                    .count(),
                2
            );
            let remaining: Vec<String> = serde_json::from_slice(&fs::read(fixture.0.join("forwards.json"))?)?;
            assert_eq!(remaining.len(), 2);
        }
    }
    Ok(())
}

#[tokio::test]
async fn t3_guided_drop_releases_forwards_but_requires_explicit_remote_settlement() -> Result {
    let fixture = Fixture::new()?;
    let connection = fixture.connection(deadline()).await?;
    prepare(&fixture, 41001, "signin")?;
    let barrier = fixture.listener("account-barrier")?;
    let cleanup = fixture.listener("cleanup-notify")?;
    let mut guided = Box::pin(connection.guide_account(AccountCommand::Signin, 100, |_| Ok(())));
    let child = tokio::select! {
        result = &mut guided => return Err(format!("early result: {result:?}").into()),
        socket = ready(&barrier) => socket?,
    };
    drop(guided);
    drop(child);
    assert_eq!(
        connection.account(AccountCommand::Link).await,
        Err(Failure("setup_account_unsettled"))
    );
    for _ in 0..2 {
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), cleanup.accept()).await??;
        let mut prefix = [0; 4];
        socket.read_exact(&mut prefix).await?;
        assert_eq!(&prefix, b"tcp:");
        socket.write_all(&[1]).await?;
    }
    fixture.settle(&connection).await?;
    let remaining: Vec<String> = serde_json::from_slice(&fs::read(fixture.0.join("forwards.json"))?)?;
    assert_eq!(remaining, Vec::<String>::new());
    fs::remove_file(fixture.0.join("account-barrier"))?;
    fs::write(fixture.0.join("account-reply"), "echo-install: link completed\n")?;
    connection.account(AccountCommand::Link).await.map_err(diagnostic)?;
    Ok(())
}

#[tokio::test]
async fn t3_guided_expired_settlement_is_cleanup_refusal_not_provider_timeout() -> Result {
    let fixture = Fixture::new()?;
    let connection = fixture.connection(deadline()).await?;
    prepare(&fixture, 41001, "signin")?;
    fs::write(fixture.0.join("account-reply"), "SYNTHETIC-TOKEN")?;
    let barrier = fixture.listener("stop-barrier")?;
    let mut guided = Box::pin(connection.guide_account(AccountCommand::Signin, 100, |_| Ok(())));
    let child = tokio::select! {
        result = &mut guided => return Err(format!("early result: {result:?}").into()),
        socket = ready(&barrier) => socket?,
    };
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(11)).await;
    tokio::time::resume();
    assert_eq!(guided.await, Err(Failure("setup_account_unsettled")));
    drop(child);
    cleaned(&fixture, true)?;
    let before = fixture.calls()?.len();
    assert_eq!(
        connection.account(AccountCommand::Link).await,
        Err(Failure("setup_timeout"))
    );
    assert_eq!(fixture.calls()?.len(), before);
    Ok(())
}
