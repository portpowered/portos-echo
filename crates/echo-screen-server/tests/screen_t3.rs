//! T3 component witnesses: real loopback HTTP with synthetic state/action boundaries.
mod screen_settings;
mod support;
use echo_screen_contract::{ScreenFrame, UiAction, UiState};
use support::{Result, TOKEN, events, frame, post, report, request, server};
use tokio::{io::AsyncReadExt, net::TcpListener};

#[cfg(echo_screen_embedded)]
#[tokio::test]
async fn t3_embedded_javascript_is_served_outside_the_checkout() -> Result {
    let expected = std::fs::read_to_string(std::env::var("ECHO_SCREEN_BUNDLE")?)?;
    let outside = std::env::var("ECHO_SCREEN_TEST_CWD")?;
    assert_eq!(std::env::current_dir()?, std::path::PathBuf::from(outside));
    let (mut server, tx) = server().await?;
    assert_eq!(
        request(server.bound_addr, "GET", "/screen.js", "", "").await?,
        (200, expected)
    );
    drop(tx);
    server.shutdown().await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn t3_interrupted_graceful_shutdown_keeps_original_deadline() -> Result {
    use std::{future::Future, task::Poll, time::Duration};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (mut server, _tx) = server().await?;
    let mut socket = tokio::net::TcpStream::connect(server.bound_addr).await?;
    socket.write_all(format!("POST /actions?token={TOKEN} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 100\r\nExpect: 100-continue\r\n\r\n").as_bytes()).await?;
    let mut socket = BufReader::new(socket);
    let mut line = String::new();
    socket.read_line(&mut line).await?;
    assert_eq!(line, "HTTP/1.1 100 Continue\r\n");
    line.clear();
    socket.read_line(&mut line).await?;
    assert_eq!(line, "\r\n");
    let start = tokio::time::Instant::now();
    {
        let closing = server.shutdown_gracefully();
        tokio::pin!(closing);
        assert!(std::future::poll_fn(|cx| Poll::Ready(closing.as_mut().poll(cx).is_pending())).await);
        tokio::time::advance(Duration::from_millis(100)).await;
    }
    assert_eq!(server.shutdown_gracefully().await?, 1);
    assert_eq!(start.elapsed(), Duration::from_millis(250));
    assert_eq!(server.shutdown_gracefully().await?, 0);
    let mut tail = String::new();
    socket.read_to_string(&mut tail).await?;
    assert_eq!(tail, "");
    report("interrupted drain: original 250 ms deadline, one stalled HTTP owner collected, repeated close=0");
    Ok(())
}

#[tokio::test]
async fn t3_terminal_publication_survives_graceful_close_200_times() -> Result {
    for iteration in 0..200 {
        let (mut server, tx) = server().await?;
        let mut stream = events(server.bound_addr).await?;
        let state = UiState::Error {
            message: format!("synthetic terminal {iteration}"),
            relink: false,
        };
        tx.send(state.clone()).await?;
        server
            .flush(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
            .await?;
        assert_eq!(server.shutdown_gracefully().await?, 0);
        assert_eq!(frame(&mut stream).await?, ScreenFrame { seq: 1, state });
        let mut tail = String::new();
        stream.read_to_string(&mut tail).await?;
        assert!(!tail.contains("data:"));
    }
    report("terminal publication then graceful close: 200/200 exact terminal frames followed by EOF");
    Ok(())
}

#[tokio::test]
async fn t3_graceful_shutdown_delivers_committed_failure_before_stream_eof() -> Result {
    let (mut server, tx) = server().await?;
    let address = server.bound_addr;
    let mut stream = events(address).await?;
    let states = [
        UiState::Listening,
        UiState::Error {
            message: "Conversation unavailable. Try another wake.".into(),
            relink: false,
        },
    ];
    for state in &states {
        tx.send(state.clone()).await?;
    }
    server
        .flush(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
        .await?;
    // Hold the observer until teardown completes: it must receive the actual
    // committed Error frame, even when no client read races server shutdown.
    assert_eq!(server.shutdown_gracefully().await?, 0);
    for (index, state) in states.into_iter().enumerate() {
        assert_eq!(
            frame(&mut stream).await?,
            ScreenFrame {
                seq: index as u64 + 1,
                state
            }
        );
    }
    let mut tail = String::new();
    stream.read_to_string(&mut tail).await?;
    assert!(!tail.contains("data:"));
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
    Ok(())
}

#[tokio::test]
async fn t3_exact_sequence_actions_and_reconnect() -> Result {
    let (mut server, tx) = server().await?;
    let mut a = events(server.bound_addr).await?;
    let mut b = events(server.bound_addr).await?;
    let states = [
        UiState::Ready {
            message: "Echo Live ready".into(),
        },
        UiState::Listening,
        UiState::Speaking,
        UiState::Ready {
            message: "Echo Live ready".into(),
        },
    ];
    for (index, state) in states.into_iter().enumerate() {
        tx.send(state.clone()).await?;
        let expected = ScreenFrame {
            seq: u64::try_from(index)? + 1,
            state,
        };
        assert_eq!(frame(&mut a).await?, expected);
        assert_eq!(frame(&mut b).await?, expected);
    }
    let mut late = events(server.bound_addr).await?;
    assert_eq!(
        frame(&mut late).await?,
        ScreenFrame {
            seq: 4,
            state: UiState::Ready {
                message: "Echo Live ready".into()
            }
        }
    );
    for action in [
        UiAction::Tap {
            target: "primary".into(),
        },
        UiAction::Approve {
            id: "approval-1".into(),
        },
        UiAction::Deny {
            id: "approval-1".into(),
        },
        UiAction::Relink,
        UiAction::Dismiss,
    ] {
        assert_eq!(
            post(server.bound_addr, &serde_json::to_string(&action)?).await?,
            (204, String::new())
        );
        assert_eq!(server.actions.recv().await, Some(action));
    }
    assert!(server.actions.try_recv().is_err());
    report(
        "F01/F02/F05/F06/F07: seq=1..4 ready/listening/speaking/ready; two clients identical; reconnect=4; five exact actions once",
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn t3_refusal_revocation_and_no_disclosure() -> Result {
    let (mut server, tx) = server().await?;
    let mut stream = events(server.bound_addr).await?;
    tx.send(UiState::Listening).await?;
    assert_eq!(frame(&mut stream).await?.state, UiState::Listening);
    for query in [
        "",
        "?token=",
        "?token=wrong",
        "?token=synthetic-screen-pair&token=synthetic-screen-pair",
        "?token=%",
        "?%74oken=wrong",
    ] {
        for (method, path) in [("GET", "/events"), ("POST", "/actions")] {
            assert_eq!(
                request(server.bound_addr, method, &format!("{path}{query}"), "bad", "").await?,
                (401, String::new())
            );
        }
    }
    server.revoke_pairing().await?;
    let mut remaining = String::new();
    stream.read_to_string(&mut remaining).await?;
    assert!(!remaining.contains("data:"));
    assert_eq!(
        request(server.bound_addr, "GET", &format!("/events?token={TOKEN}"), "", "").await?,
        (401, String::new())
    );
    assert_eq!(
        post(server.bound_addr, r#"{"action":"dismiss"}"#).await?,
        (401, String::new())
    );
    assert!(server.actions.try_recv().is_err());
    report("F03/F04: missing/wrong/empty/duplicate/revoked=401; state body bytes=0; actions=0; active stream EOF");
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn t3_validation_action_backpressure_and_origin() -> Result {
    let (mut server, _tx) = server().await?;
    let path = format!("/actions?token={TOKEN}");
    for body in ["{", r#"{"action":"unknown"}"#, r#"{"action":"approve"}"#] {
        assert_eq!(post(server.bound_addr, body).await?, (400, String::new()));
    }
    assert_eq!(
        request(server.bound_addr, "POST", &path, "{}", "Content-Type: text/plain\r\n")
            .await?
            .0,
        415
    );
    assert_eq!(post(server.bound_addr, &"x".repeat(65537)).await?.0, 413);
    let action = r#"{"action":"dismiss"}"#;
    assert_eq!(
        request(
            server.bound_addr,
            "POST",
            &path,
            action,
            "Content-Type: application/json\r\nOrigin: http://127.0.0.1:1\r\n"
        )
        .await?
        .0,
        403
    );
    assert!(server.actions.try_recv().is_err());
    for _ in 0..32 {
        assert_eq!(post(server.bound_addr, action).await?.0, 204);
    }
    assert_eq!(post(server.bound_addr, action).await?.0, 503);
    assert_eq!(server.actions.recv().await, Some(UiAction::Dismiss));
    assert_eq!(post(server.bound_addr, action).await?.0, 204);
    server.actions.close();
    assert_eq!(post(server.bound_addr, action).await?.0, 503);
    report(
        "F08/F09/F17: invalid=400; media=415; 65537 bytes=413; foreign Origin=403; capacity=32; overflow/closed=503; manual retry=204",
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn t3_stream_capacity_assets_and_cleanup() -> Result {
    let (mut server, tx) = server().await?;
    let addr = server.bound_addr;
    let mut streams = Vec::new();
    for _ in 0..32 {
        streams.push(events(addr).await?);
    }
    assert_eq!(
        request(addr, "GET", &format!("/events?token={TOKEN}"), "", "").await?.0,
        503
    );
    for (path, source) in [
        ("/", include_str!("../../../web/screen/src/index.html")),
        ("/screen.css", include_str!("../../../web/screen/src/screen.css")),
    ] {
        assert_eq!(request(addr, "GET", path, "", "").await?, (200, source.into()));
    }
    for path in ["/missing", "/../Cargo.toml", "/src/index.html"] {
        assert_eq!(request(addr, "GET", path, "", "").await?.0, 404);
    }
    assert_eq!(request(addr, "POST", "/", "", "").await?.0, 405);
    assert_eq!(
        server.actions.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    );
    drop(tx);
    for mut stream in streams {
        let mut tail = String::new();
        stream.read_to_string(&mut tail).await?;
        assert!(!tail.contains("data:"));
    }
    server.shutdown().await?;
    assert_eq!(server.actions.recv().await, None);
    // The action senders belong to the listener router and its connections.
    // screen_listener::run drops the listener before releasing that router.
    report(
        "F10/F12/F16: streams=32; excess=503; static bytes exact; unknown=404; method=405; input close EOF/actions EOF after listener task join",
    );
    Ok(())
}

#[tokio::test]
async fn t3_constructor_failures_and_owner_cleanup() -> Result {
    use echo_contracts::secret::Secret;
    use echo_screen_server::{ScreenServerError, serve};
    use std::net::{Ipv4Addr, SocketAddr};
    for mode in ["shutdown", "drop"] {
        let (mut server, _tx) = server().await?;
        let addr = server.bound_addr;
        let mut stream = events(addr).await?;
        let mut actions = std::mem::replace(&mut server.actions, tokio::sync::mpsc::channel(1).1);
        assert_eq!(actions.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty));
        if mode == "shutdown" {
            server.shutdown().await?;
        }
        drop(server);
        let mut tail = String::new();
        stream.read_to_string(&mut tail).await?;
        assert!(!tail.contains("data:"));
        // EOF of this instance's action channel observes destruction of its
        // router after the listener drop, including the owner-Drop path.
        assert_eq!(actions.recv().await, None);
    }
    let occupied = TcpListener::bind("127.0.0.1:0").await?;
    let (_, rx) = tokio::sync::mpsc::channel(32);
    let error = serve(occupied.local_addr()?, Secret::new(TOKEN.into()), rx)
        .await
        .err()
        .ok_or("bind succeeded")?;
    assert!(matches!(error, ScreenServerError::Bind));
    assert_eq!(error.to_string(), "screen listener could not bind");
    let (_, rx) = tokio::sync::mpsc::channel(32);
    let error = serve(
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        Secret::new(TOKEN.into()),
        rx,
    )
    .await
    .err()
    .ok_or("non-loopback accepted")?;
    assert!(matches!(error, ScreenServerError::InvalidBind));
    let (_, rx) = tokio::sync::mpsc::channel(32);
    let error = serve("127.0.0.1:0".parse()?, Secret::new(String::new()), rx)
        .await
        .err()
        .ok_or("empty token accepted")?;
    assert!(matches!(error, ScreenServerError::InvalidToken));
    report(
        "F12/F13: explicit shutdown/Drop stream EOF + router-owned actions EOF; occupied=Bind; non-loopback=InvalidBind; empty=InvalidToken",
    );
    Ok(())
}
