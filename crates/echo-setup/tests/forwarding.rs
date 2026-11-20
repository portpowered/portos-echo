//! T3 resource ownership: real adb child processes, per-case ledgers and loopback
//! barriers. No real adb, device, account or provider; no environment mutation.
use echo_setup::{
    Failure,
    cli::Options,
    setup::{ForwardSession, InstalledConnection},
};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::time::Instant;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new(replies: &[&str]) -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "echo-forward-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_echo-setup-test-adb"), path.join("adb.exe"))?;
        #[cfg(not(unix))]
        fs::copy(env!("CARGO_BIN_EXE_echo-setup-test-adb"), path.join("adb.exe"))?;
        fs::write(path.join("devices"), "SYNTHETIC-DEVICE\tdevice\r\n")?;
        fs::write(path.join("forward-replies.json"), serde_json::to_vec(replies)?)?;
        fs::write(path.join("forwards.json"), br#"["tcp:49999"]"#)?; // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
        fs::write(path.join("forward-targets.json"), br#"{"tcp:49999":"tcp:1"}"#)?; // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
        Ok(Self(path))
    }

    fn options(&self) -> Result<Options> {
        Options::parse(["status".into(), "--adb".into(), self.0.join("adb.exe").into_os_string()])
            .map_err(|error| format!("{error:?}").into())
    }

    fn active(&self) -> Result<Vec<String>> {
        Ok(serde_json::from_slice(&fs::read(self.0.join("forwards.json"))?)?)
    }

    fn ledger(&self) -> Result<Vec<Vec<String>>> {
        fs::read_to_string(self.0.join("ledger"))?
            .lines()
            .map(|line| serde_json::from_str(line).map_err(Into::into))
            .collect()
    }

    fn listener(&self, name: &str) -> Result<TcpListener> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        fs::write(self.0.join(name), listener.local_addr()?.port().to_string())?;
        Ok(TcpListener::from_std(listener)?)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn fixed(error: Failure) -> String {
    let text = format!("{error:?}");
    for marker in [
        "SYNTHETIC-DEVICE",
        "SYNTHETIC-TOKEN",
        "SYNTHETIC-ACCOUNT",
        "SYNTHETIC-PRIVATE-PATH",
    ] {
        assert!(!text.contains(marker));
    }
    text
}

#[tokio::test]
async fn t3_forwards_allocate_without_rebind_and_close_only_their_own_ports() -> Result {
    let fixture = Fixture::new(&["41001\r\n", "41002\r\n"])?;
    fs::write(
        fixture.0.join("setup-config"),
        include_str!("../../../examples/bridge.echo.toml"),
    )?;
    fs::write(fixture.0.join("setup-pairing"), "SYNTHETIC-PAIR\r\n")?;
    let connection = InstalledConnection::read(&fixture.options()?, deadline())
        .await
        .map_err(fixed)?;
    fs::write(fixture.0.join("devices"), "SYNTHETIC-OTHER\tdevice\n")?;
    let session = connection.forward().await.map_err(fixed)?;
    assert_eq!(fixture.active()?, ["tcp:49999", "tcp:41001", "tcp:41002"]); // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
    session.close().await.map_err(fixed)?;
    assert_eq!(fixture.active()?, ["tcp:49999"]); // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
    let calls = fixture.ledger()?;
    let forwarded: Vec<_> = calls
        .iter()
        .filter(|args| args.get(2).is_some_and(|arg| arg == "forward"))
        .collect();
    assert_eq!(forwarded.len(), 6);
    for (args, expected) in forwarded.iter().zip([
        vec!["forward", "--no-rebind", "tcp:0", "tcp:16900"], // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
        vec!["forward", "--no-rebind", "tcp:0", "tcp:16901"], // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
        vec!["forward", "--list"],
        vec!["forward", "--remove", "tcp:41002"], // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
        vec!["forward", "--list"],
        vec!["forward", "--remove", "tcp:41001"], // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
    ]) {
        assert_eq!(args[0], "-s");
        assert_eq!(args[1], "SYNTHETIC-DEVICE");
        assert_eq!(args[2..], expected);
    }
    Ok(())
}

#[tokio::test]
async fn t3_forward_lease_routes_real_health_and_authenticated_screen_requests() -> Result {
    let health = TcpListener::bind("127.0.0.1:0").await?;
    let screen = TcpListener::bind("127.0.0.1:0").await?;
    let fixture = Fixture::new(&[
        &format!("{}\n", screen.local_addr()?.port()),
        &format!("{}\n", health.local_addr()?.port()),
    ])?;
    fs::write(
        fixture.0.join("setup-config"),
        include_str!("../../../examples/bridge.echo.toml"),
    )?;
    fs::write(fixture.0.join("setup-pairing"), "SYNTHETIC-PAIR\r\n")?;
    let connection = InstalledConnection::read(&fixture.options()?, deadline())
        .await
        .map_err(fixed)?;
    let session = connection.forward().await.map_err(fixed)?;
    let body =
        r#"{"status":"running","wyoming":"connected","live":"idle","relink_required":false,"kill_switch":false}"#;
    let health_server = tokio::spawn(async move { serve(&health, "/healthz", "application/json", body).await });
    assert!(session.health(deadline()).await.map_err(fixed)?.runtime_ready());
    health_server.await??;
    let body = "data: {\"seq\":1,\"state\":{\"state\":\"link_code\",\"user_code\":\"TEST-LINK\",\"verification_uri\":\"https://auth.portoperatingsystem.com/device\",\"expires_at_unix\":120}}\n\n"; // lint-rules: allow-literal inert screen fixture; requests only connect to loopback
    let screen_server =
        tokio::spawn(async move { serve(&screen, "/events?token=SYNTHETIC-PAIR", "text/event-stream", body).await });
    let secret = connection.pairing().await.map_err(fixed)?;
    let mut stream = session.screen(&secret, deadline()).await.map_err(fixed)?;
    let prompt = stream
        .next_prompt(echo_setup::setup::Provider::Portos, 100)
        .await
        .map_err(fixed)?;
    let value = serde_json::to_value(prompt)?;
    assert_eq!(value["user_code"], "TEST-LINK");
    assert_eq!(value["provider"], "portos");
    assert!(!value.to_string().contains("SYNTHETIC-PAIR"));
    drop(stream);
    screen_server.await??;
    session.close().await.map_err(fixed)?;
    assert_eq!(fixture.active()?, ["tcp:49999"]); // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
    Ok(())
}

async fn serve(listener: &TcpListener, path: &str, mime: &str, body: &str) -> Result {
    let mut socket = accept(listener).await?;
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        tokio::time::timeout(Duration::from_secs(5), socket.read_exact(&mut byte)).await??;
        request.push(byte[0]);
    }
    assert_eq!(
        String::from_utf8(request)?.lines().next(),
        Some(format!("GET {path} HTTP/1.1").as_str())
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await?;
    Ok(())
}

#[tokio::test]
async fn t3_partial_forward_allocation_failure_unwinds_the_first_allocation() -> Result {
    for reply in ["FAIL", "SYNTHETIC-TOKEN\n", "0\n", "41001\n"] {
        let fixture = Fixture::new(&["41001\n", reply])?;
        let cleanup = fixture.listener("cleanup-notify")?;
        let witness = tokio::spawn(async move { notification(&cleanup, "tcp:41001").await }); // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
        let result = ForwardSession::open(&fixture.options()?, [16900, 16901], deadline()).await;
        assert_eq!(result.err(), Some(Failure("setup_forward_failed")));
        witness.await??;
        assert_eq!(fixture.active()?, ["tcp:49999"]); // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
    }
    Ok(())
}

async fn accept(listener: &TcpListener) -> Result<tokio::net::TcpStream> {
    let (socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept()).await??;
    Ok(socket)
}

async fn notification(listener: &TcpListener, expected: &str) -> Result {
    let mut socket = accept(listener).await?;
    let mut bytes = vec![0; expected.len()];
    tokio::time::timeout(Duration::from_secs(5), socket.read_exact(&mut bytes)).await??;
    assert_eq!(bytes, expected.as_bytes());
    socket.write_all(&[1]).await?;
    Ok(())
}

#[tokio::test]
async fn t3_forward_drop_and_inflight_cancellation_still_release_owned_allocations() -> Result {
    let fixture = Fixture::new(&["41001\n", "41002\n"])?;
    let cleanup = fixture.listener("cleanup-notify")?;
    let session = ForwardSession::open(&fixture.options()?, [16900, 16901], deadline())
        .await
        .map_err(fixed)?;
    drop(session);
    notification(&cleanup, "tcp:41002").await?; // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
    notification(&cleanup, "tcp:41001").await?; // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
    assert_eq!(fixture.active()?, ["tcp:49999"]); // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
    cancelled_allocation(false).await?;
    cancelled_allocation(true).await
}

async fn cancelled_allocation(expire: bool) -> Result {
    let fixture = Fixture::new(&["41001\n", "41002\n"])?;
    let barrier = fixture.listener("allocation-barrier")?;
    let cleanup = fixture.listener("cleanup-notify")?;
    let options = fixture.options()?;
    let end = Instant::now() + Duration::from_secs(5);
    let mut pending = Box::pin(ForwardSession::open(&options, [16900, 16901], end));
    let mut socket = tokio::select! {
        biased;
        _ = &mut pending => return Err("allocation completed before barrier".into()),
        ready = accept(&barrier) => ready?,
    };
    let mut bytes = [0; 5];
    socket.read_exact(&mut bytes).await?;
    assert_eq!(&bytes, b"ready");
    if expire {
        expire_before_cleanup(pending.as_mut()).await?;
        socket.write_all(&[1]).await?;
        let (result, observed) = tokio::join!(pending, notification(&cleanup, "tcp:41001")); // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
        observed?;
        assert_eq!(result.err(), Some(Failure("setup_timeout")));
    } else {
        drop(pending);
        socket.write_all(&[1]).await?;
        notification(&cleanup, "tcp:41001").await?; // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
    }
    assert_eq!(fixture.active()?, ["tcp:49999"]); // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
    let remaining: Value = serde_json::from_slice(&fs::read(fixture.0.join("forward-replies.json"))?)?;
    assert_eq!(remaining, serde_json::json!(["41002\n"]));
    Ok(())
}

async fn expire_before_cleanup<F: std::future::Future>(pending: std::pin::Pin<&mut F>) -> Result {
    tokio::time::pause();
    // Cross the millisecond timer tick while staying below adb's ten-second
    // subprocess ceiling. This is virtual time, not a scheduling sleep.
    tokio::time::advance(Duration::from_secs(6)).await;
    // Expiry requests cleanup; it cannot report a terminal failure until
    // the in-flight allocation has returned and its port has been removed.
    tokio::select! {
        biased;
        _ = pending => return Err("deadline returned before owned cleanup".into()),
        () = std::future::ready(()) => {}
    }
    tokio::time::resume();
    Ok(())
}

#[tokio::test]
async fn t3_forward_cleanup_failure_is_reported_without_removing_unrelated_forwards() -> Result {
    let fixture = Fixture::new(&["41001\n", "41002\n"])?;
    let session = ForwardSession::open(&fixture.options()?, [16900, 16901], deadline())
        .await
        .map_err(fixed)?;
    fs::write(fixture.0.join("remove-fails"), "")?;
    let failure = session.close().await.err().ok_or("cleanup unexpectedly succeeded")?;
    assert_eq!(fixed(failure), "Failure(\"setup_forward_cleanup_failed\")");
    assert_eq!(fixture.active()?, ["tcp:49999", "tcp:41001", "tcp:41002"]); // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
    let calls = fixture.ledger()?;
    assert_eq!(
        calls
            .iter()
            .filter(|args| args.iter().any(|value| value == "--remove"))
            .count(),
        2
    );
    Ok(())
}

#[tokio::test]
async fn t3_forward_cleanup_refuses_a_replaced_mapping_and_still_cleans_other_owned_ports() -> Result {
    let fixture = Fixture::new(&["41001\n", "41002\n"])?;
    let session = ForwardSession::open(&fixture.options()?, [16900, 16901], deadline())
        .await
        .map_err(fixed)?;
    fs::write(
        fixture.0.join("forward-targets.json"),
        br#"{"tcp:49999":"tcp:1","tcp:41001":"tcp:16900","tcp:41002":"tcp:12345"}"#, // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
    )?;
    assert_eq!(session.close().await, Err(Failure("setup_forward_cleanup_failed")));
    assert_eq!(fixture.active()?, ["tcp:49999", "tcp:41002"]); // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
    let calls = fixture.ledger()?;
    let removed: Vec<_> = calls
        .iter()
        .filter(|args| args.iter().any(|value| value == "--remove"))
        .collect();
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].last().map(String::as_str), Some("tcp:41001")); // lint-rules: allow-literal inert adb forward selector fixture; sockets bind literal loopback only
    Ok(())
}

#[tokio::test]
async fn t3_invalid_forward_inputs_and_expired_deadlines_have_no_adb_effects() -> Result {
    let fixture = Fixture::new(&[])?;
    let options = fixture.options()?;
    for remote in [[0, 16901], [16900, 0]] {
        assert_eq!(
            ForwardSession::open(&options, remote, deadline()).await.err(),
            Some(Failure("setup_forward_failed"))
        );
    }
    assert_eq!(
        ForwardSession::open(&options, [16900, 16901], Instant::now())
            .await
            .err(),
        Some(Failure("setup_timeout"))
    );
    assert!(!fixture.0.join("ledger").exists());
    Ok(())
}

#[tokio::test]
async fn t3_private_connection_refuses_bad_config_and_never_reads_account_files() -> Result {
    let base = include_str!("../../../examples/bridge.echo.toml");
    for raw in [
        "SYNTHETIC-TOKEN".to_owned(),
        base.replace("screen-pairing.txt", "codex-home/auth.json"),
        base.replace("127.0.0.1:16900", "127.0.0.1:0"),
        format!("{base}\n[screen]\nlisten = '127.0.0.1:16900'\n"),
    ] {
        let fixture = Fixture::new(&[])?;
        fs::write(fixture.0.join("setup-config"), raw)?;
        let error = InstalledConnection::read(&fixture.options()?, deadline())
            .await
            .err()
            .ok_or("accepted config")?;
        assert_eq!(fixed(error), "Failure(\"setup_config_invalid\")");
        let calls = fixture.ledger()?;
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[2][3], "sh -s -- --device setup-config");
        assert_eq!(
            fs::read(fixture.0.join("adapter-input"))?,
            echo_setup::DEVICE_SCRIPT.as_bytes()
        );
    }
    Ok(())
}

#[tokio::test]
async fn t3_private_pairing_stays_on_selected_device_and_has_fixed_errors() -> Result {
    let fixture = Fixture::new(&[])?;
    fs::write(
        fixture.0.join("setup-config"),
        include_str!("../../../examples/bridge.echo.toml"),
    )?;
    let connection = InstalledConnection::read(&fixture.options()?, deadline())
        .await
        .map_err(fixed)?;
    fs::write(fixture.0.join("devices"), "SYNTHETIC-OTHER\tdevice\n")?;
    for value in ["SYNTHETIC-PAIR\n", "SYNTHETIC-PAIR\r\n"] {
        fs::write(fixture.0.join("setup-pairing"), value)?;
        let pair = connection.pairing().await.map_err(fixed)?;
        assert_eq!(pair.expose(), "SYNTHETIC-PAIR");
    }
    for value in ["", " \r\n", "SYNTHETIC-TOKEN\nSYNTHETIC-ACCOUNT", &"x".repeat(16_385)] {
        fs::write(fixture.0.join("setup-pairing"), value)?;
        let error = connection.pairing().await.err().ok_or("accepted pairing")?;
        assert_eq!(fixed(error), "Failure(\"setup_pairing_invalid\")");
    }
    fs::write(fixture.0.join("operation-failed"), "")?;
    let error = connection.pairing().await.err().ok_or("ignored dependency failure")?;
    assert_eq!(fixed(error), "Failure(\"setup_private_unavailable\")");
    for args in fixture.ledger()?.iter().skip(1) {
        assert_eq!(args[0..2], ["-s", "SYNTHETIC-DEVICE"]);
        assert!(!args.iter().any(|arg| arg.contains("auth.json")));
    }
    Ok(())
}

#[tokio::test]
async fn t3_private_read_deadline_and_cancellation_close_owned_child() -> Result {
    let fixture = Fixture::new(&[])?;
    let options = fixture.options()?;
    assert_eq!(
        InstalledConnection::read(&options, Instant::now()).await.err(),
        Some(Failure("setup_timeout"))
    );
    assert!(!fixture.0.join("ledger").exists());
    fs::write(
        fixture.0.join("setup-config"),
        include_str!("../../../examples/bridge.echo.toml"),
    )?;
    for expire in [false, true] {
        let listener = fixture.listener("private-barrier")?;
        let mut pending = Box::pin(InstalledConnection::read(
            &options,
            Instant::now() + Duration::from_secs(5),
        ));
        let mut socket = tokio::select! {
            biased;
            result = &mut pending => return Err(format!("read completed before barrier: {:?}", result.err()).into()),
            ready = accept(&listener) => ready?,
        };
        let mut bytes = [0; 5];
        socket.read_exact(&mut bytes).await?;
        assert_eq!(&bytes, b"ready");
        if expire {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(6)).await;
            let error = pending.await.err().ok_or("expired read succeeded")?;
            tokio::time::resume();
            assert_eq!(fixed(error), "Failure(\"setup_timeout\")");
        } else {
            drop(pending);
        }
        let mut byte = [0];
        let closed = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut byte)).await?;
        assert!(
            matches!(closed, Ok(0))
                || closed
                    .as_ref()
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionReset)
        );
    }
    Ok(())
}
