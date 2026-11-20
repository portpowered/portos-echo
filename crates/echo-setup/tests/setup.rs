//! T3 component HTTP witness. Real observer + loopback HTTP; no devices/accounts.
//! This does not stand in for the retained CLI setup/forward/wake journey.
use echo_contracts::secret::Secret;
use echo_setup::Failure;
use echo_setup::setup::{Provider, ScreenEvents, health, link_required};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tokio::time::Instant;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Server {
    port: u16,
    request: mpsc::Receiver<String>,
    worker: thread::JoinHandle<std::io::Result<()>>,
}

impl Server {
    fn one(status: &str, headers: &str, body: &str, hold: bool) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let (send, request) = mpsc::channel();
        let response = format!("HTTP/1.1 {status}\r\n{headers}\r\n{body}");
        let worker = thread::spawn(move || {
            let (mut socket, _) = listener.accept()?;
            socket.set_read_timeout(Some(Duration::from_secs(5)))?;
            socket.set_write_timeout(Some(Duration::from_secs(5)))?;
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte)?;
                request.push(byte[0]);
                if request.len() > 32_768 {
                    return Err(std::io::Error::other("request bound"));
                }
            }
            send.send(String::from_utf8_lossy(&request).into_owned())
                .map_err(|_| std::io::Error::other("request receiver closed"))?;
            socket.write_all(response.as_bytes())?;
            if hold {
                let mut byte = [0];
                // The observer's drop must close its pending HTTP body.
                assert_eq!(socket.read(&mut byte)?, 0);
            }
            Ok(())
        });
        Ok(Self { port, request, worker })
    }

    fn finish(self, path: &str) -> Result {
        let request = self.request.recv_timeout(Duration::from_secs(5))?;
        let line = request.lines().next().ok_or("request absent")?;
        assert_eq!(line, format!("GET {path} HTTP/1.1"));
        self.worker.join().map_err(|_| "server failed")??;
        Ok(())
    }
}

fn pairing() -> Secret<String> {
    Secret::new("SYNTHETIC-PAIR +&?".into())
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
fn diagnostic(failure: Failure) -> String {
    format!("{failure:?}")
}

fn event(seq: u64, state: Value) -> String {
    let mut frame = json!({"seq":seq});
    frame["state"] = state;
    format!("data: {frame}\r\n\r\n")
}

fn screen(body: &str, hold: bool) -> Result<Server> {
    Server::one(
        "200 OK",
        &format!("Content-Type: text/event-stream\r\nContent-Length: {}\r\n", body.len()),
        body,
        hold,
    )
}

const EVENTS: &str = "/events?token=SYNTHETIC-PAIR+%2B%26%3F";

#[tokio::test]
async fn t3_setup_reads_actual_screen_shapes_and_deduplicates_presented_prompts() -> Result {
    let state = json!({"state":"ready", "message":"code: TEST-CODE\nurl: https://auth.openai.com/codex/device\n", // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
        "token":"SYNTHETIC-TOKEN", "account_id":"SYNTHETIC-ACCOUNT"});
    let body = event(1, json!({"state":"connecting"}))
        + &event(2, state.clone())
        + &event(3, state)
        + &event(
            4,
            json!({"state":"ready", "message":"code: NEXT-CODE\nurl: https://auth.openai.com/codex/device\n"}), // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
        );
    let server = screen(&body, false)?;
    let mut stream = ScreenEvents::connect(server.port, &pairing(), deadline())
        .await
        .map_err(diagnostic)?;
    let prompt = stream.next_prompt(Provider::Chatgpt, 100).await.map_err(diagnostic)?;
    let public = serde_json::to_value(&prompt)?;
    assert_eq!(
        public,
        json!({"type":"prompt", "provider":"chatgpt",
        "user_code":"TEST-CODE", "verification_uri":"https://auth.openai.com/codex/device"}) // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
    );
    assert!(!public.to_string().contains("SYNTHETIC"));
    stream.presented(prompt);
    let next = stream.next_prompt(Provider::Chatgpt, 100).await.map_err(diagnostic)?;
    assert_eq!(serde_json::to_value(next)?["user_code"], "NEXT-CODE");
    assert_eq!(
        stream.next_prompt(Provider::Chatgpt, 100).await.err(),
        Some(Failure("setup_screen_closed"))
    );
    drop(stream);
    server.finish(EVENTS)
}

#[tokio::test]
async fn t3_setup_portos_preserves_public_code_and_rejects_expired_and_provider_error() -> Result {
    for (state, expected) in [
        (
            json!({"state":"link_code", "user_code":"TEST-LINK",
            "verification_uri":"https://auth.portoperatingsystem.com/device", "expires_at_unix":120}), // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
            None,
        ),
        (
            json!({"state":"link_code", "user_code":"TEST-LINK",
            "verification_uri":"https://auth.portoperatingsystem.com/device", "expires_at_unix":100}), // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
            Some(Failure("setup_code_expired")),
        ),
        (
            json!({"state":"error", "message":"SYNTHETIC-TOKEN SYNTHETIC-ACCOUNT", "relink":true}),
            Some(Failure("setup_provider_failed")),
        ),
    ] {
        let server = screen(&event(1, state), false)?;
        let mut stream = ScreenEvents::connect(server.port, &pairing(), deadline())
            .await
            .map_err(diagnostic)?;
        let result = stream.next_prompt(Provider::Portos, 100).await;
        if let Some(error) = expected {
            assert_eq!(result.err(), Some(error));
            assert!(!diagnostic(error).contains("SYNTHETIC"));
        } else {
            assert_eq!(
                serde_json::to_value(result.map_err(diagnostic)?)?,
                json!({"type":"prompt",
                "provider":"portos", "user_code":"TEST-LINK", "verification_uri":"https://auth.portoperatingsystem.com/device"}) // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
            );
        }
        drop(stream);
        server.finish(EVENTS)?;
    }
    Ok(())
}

#[tokio::test]
async fn t3_setup_refuses_redirect_unauthorized_wrong_mime_and_malformed_event() -> Result {
    // A redirect back to the same listener would need a second connection; the
    // single-request server proves it is refused instead of followed.
    for (status, headers, body, error) in [
        (
            "302 Found",
            "Location: /other?token=SYNTHETIC-TOKEN\r\nContent-Length: 0\r\n",
            "",
            "setup_observation_failed",
        ),
        (
            "401 Unauthorized",
            "Content-Length: 0\r\n",
            "",
            "setup_observation_failed",
        ),
        (
            "200 OK",
            "Content-Length: 0\r\nContent-Type: application/json\r\n",
            "",
            "setup_screen_invalid",
        ),
        (
            "200 OK",
            "Content-Length: 13\r\nContent-Type: text/event-stream\r\n",
            "data: bad!!\n\n",
            "setup_screen_invalid",
        ),
    ] {
        let server = Server::one(status, headers, body, false)?;
        let result = match ScreenEvents::connect(server.port, &pairing(), deadline()).await {
            Ok(mut stream) => stream.next_prompt(Provider::Chatgpt, 100).await.err(),
            Err(error) => Some(error),
        };
        assert_eq!(result, Some(Failure(error)));
        server.finish(EVENTS)?;
    }
    Ok(())
}

#[tokio::test]
async fn t3_setup_fixed_deadline_closes_owned_stream_without_success_on_silent_peer() -> Result {
    let server = Server::one(
        "200 OK",
        "Content-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n",
        "",
        true,
    )?;
    let end = deadline();
    let mut stream = ScreenEvents::connect(server.port, &pairing(), end)
        .await
        .map_err(diagnostic)?;
    tokio::time::pause();
    {
        let wait = stream.next_prompt(Provider::Chatgpt, 100);
        tokio::pin!(wait);
        // Observe a pending wait, then explicitly move the fake clock to its
        // absolute deadline. No host scheduling or timer-resolution assertion.
        tokio::select! {
            biased;
            _ = &mut wait => return Err("silent stream completed before the deadline".into()),
            () = std::future::ready(()) => {}
        }
        tokio::time::advance(end.saturating_duration_since(Instant::now())).await;
        assert_eq!(wait.await.err(), Some(Failure("setup_timeout")));
    }
    drop(stream);
    // Keep polling the HTTP connection driver while the external peer observes
    // cancellation, rather than blocking this current-thread Tokio runtime.
    tokio::task::spawn_blocking(move || server.finish(EVENTS).map_err(|error| error.to_string())).await??;
    Ok(())
}

#[tokio::test]
async fn t3_setup_health_is_read_only_bounded_and_never_proves_fresh_wake() -> Result {
    for (wyoming, live, relink, kill, linked, connected) in [
        ("connected", "idle", false, false, true, false),
        ("connected", "connected", false, false, true, true),
        ("connecting", "connected", false, false, false, false),
        ("connected", "connected", true, false, false, false),
        ("connected", "connected", false, true, false, false),
    ] {
        let body = json!({"status":"running", "wyoming":wyoming, "live":live, "relink_required":relink,
            "kill_switch":kill, "future_private_field":"SYNTHETIC-TOKEN"})
        .to_string();
        let server = Server::one("200 OK", &format!("Content-Length: {}\r\n", body.len()), &body, false)?;
        let observation = health(server.port, deadline()).await.map_err(diagnostic)?;
        assert_eq!(observation.runtime_ready(), linked);
        assert_eq!(observation.live_connected(), connected);
        server.finish("/healthz")?;
    }
    for body in ["{}", "{\"status\":\"SYNTHETIC-ACCOUNT\"}"] {
        let server = Server::one("200 OK", &format!("Content-Length: {}\r\n", body.len()), body, false)?;
        assert_eq!(
            health(server.port, deadline()).await.err(),
            Some(Failure("setup_health_invalid"))
        );
        server.finish("/healthz")?;
    }
    Ok(())
}

#[tokio::test]
async fn t3_setup_health_body_limit_and_deadline_are_enforced() -> Result {
    let body = "x".repeat(16_385);
    let server = Server::one("200 OK", &format!("Content-Length: {}\r\n", body.len()), &body, false)?;
    assert_eq!(
        health(server.port, deadline()).await.err(),
        Some(Failure("setup_health_invalid"))
    );
    server.finish("/healthz")?;
    assert_eq!(health(1, Instant::now()).await.err(), Some(Failure("setup_timeout")));
    for token in ["", "SYNTHETIC\nTOKEN"] {
        assert_eq!(
            ScreenEvents::connect(1, &Secret::new(token.into()), deadline())
                .await
                .err(),
            Some(Failure("setup_pairing_invalid"))
        );
    }
    Ok(())
}

#[tokio::test]
async fn t3_settings_link_projection_is_authenticated_and_refuses_ambiguous_status() -> Result {
    for (body, expected) in [
        (
            r#"{"version":1,"link_status":"linked","future":"SYNTHETIC-TOKEN"}"#,
            Ok(false),
        ),
        (r#"{"version":1,"link_status":"link_required"}"#, Ok(true)),
        (
            r#"{"version":1,"link_status":"unconfigured"}"#,
            Err(Failure("setup_link_unconfigured")),
        ),
        (
            r#"{"version":2,"link_status":"linked"}"#,
            Err(Failure("setup_settings_invalid")),
        ),
        (
            r#"{"version":1,"link_status":"SYNTHETIC-ACCOUNT"}"#,
            Err(Failure("setup_settings_invalid")),
        ),
        (
            r#"{"version":1,"link_status":"linked","link_status":"link_required"}"#,
            Err(Failure("setup_settings_invalid")),
        ),
        (r#"{"link_status":"linked"}"#, Err(Failure("setup_settings_invalid"))),
        ("{}", Err(Failure("setup_settings_invalid"))),
    ] {
        let server = Server::one("200 OK", &format!("Content-Length: {}\r\n", body.len()), body, false)?;
        assert_eq!(link_required(server.port, &pairing(), deadline()).await, expected);
        server.finish("/settings/v1?token=SYNTHETIC-PAIR+%2B%26%3F")?;
    }
    Ok(())
}

#[tokio::test]
async fn t3_settings_refuses_redirect_auth_failure_oversize_and_expired_admission() -> Result {
    for status in ["302 Found", "401 Unauthorized"] {
        let server = Server::one(status, "Location: /other\r\nContent-Length: 0\r\n", "", false)?;
        assert_eq!(
            link_required(server.port, &pairing(), deadline()).await,
            Err(Failure("setup_observation_failed"))
        );
        server.finish("/settings/v1?token=SYNTHETIC-PAIR+%2B%26%3F")?;
    }
    let body = "x".repeat(262_145);
    let server = Server::one("200 OK", &format!("Content-Length: {}\r\n", body.len()), &body, false)?;
    assert_eq!(
        link_required(server.port, &pairing(), deadline()).await,
        Err(Failure("setup_settings_invalid"))
    );
    server.finish("/settings/v1?token=SYNTHETIC-PAIR+%2B%26%3F")?;
    assert_eq!(
        link_required(1, &pairing(), Instant::now()).await,
        Err(Failure("setup_timeout"))
    );
    assert_eq!(
        link_required(1, &Secret::new("bad\ntoken".into()), deadline()).await,
        Err(Failure("setup_pairing_invalid"))
    );
    Ok(())
}
