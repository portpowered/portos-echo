//! T3 account transport: prebuilt native recording adb and isolated loopback
//! barriers, with no accounts, device or provider traffic. Remote settlement
//! uses the authored stop boundary; actual Android termination remains unproved.
use echo_setup::{
    Failure,
    cli::Options,
    setup::{AccountCommand, InstalledConnection, Provider},
};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::Instant,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
static NEXT: AtomicU64 = AtomicU64::new(0);

#[path = "support/guided.rs"]
mod guided;

#[path = "support/workflow.rs"]
mod workflow;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "echo-account-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_echo-setup-test-adb"), path.join("adb.exe"))?;
        #[cfg(not(unix))]
        fs::copy(env!("CARGO_BIN_EXE_echo-setup-test-adb"), path.join("adb.exe"))?;
        fs::write(path.join("devices"), "SYNTHETIC-DEVICE\tdevice\r\n")?;
        fs::write(
            path.join("setup-config"),
            include_str!("../../../examples/bridge.echo.toml"),
        )?;
        Ok(Self(path))
    }

    async fn connection(&self, end: Instant) -> Result<InstalledConnection> {
        let options = Options::parse(["status".into(), "--adb".into(), self.0.join("adb.exe").into_os_string()])
            .map_err(diagnostic)?;
        InstalledConnection::read(&options, end)
            .await
            .map_err(|error| diagnostic(error).into())
    }

    async fn settle(&self, connection: &InstalledConnection) -> Result {
        fs::write(self.0.join("stop-reply"), "echo-install: stopped\r\n")?;
        connection.settle_account(deadline()).await.map_err(diagnostic)?;
        Ok(())
    }

    fn listener(&self, name: &str) -> Result<TcpListener> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        fs::write(self.0.join(name), listener.local_addr()?.port().to_string())?;
        Ok(TcpListener::from_std(listener)?)
    }

    fn calls(&self) -> Result<Vec<Vec<String>>> {
        fs::read_to_string(self.0.join("ledger"))?
            .lines()
            .map(|line| serde_json::from_str(line).map_err(Into::into))
            .collect()
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

fn diagnostic(error: Failure) -> String {
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
async fn t3_account_commands_use_fixed_argv_embedded_shell_and_retained_selection() -> Result {
    let fixture = Fixture::new()?;
    let connection = fixture.connection(deadline()).await?;
    fs::write(fixture.0.join("devices"), "SYNTHETIC-OTHER\tdevice\n")?;
    for (command, verb) in [
        (AccountCommand::EnsureSignin, "ensure-signin"),
        (AccountCommand::Signin, "signin"),
        (AccountCommand::Link, "link"),
    ] {
        for newline in ["\n", "\r\n"] {
            fs::write(
                fixture.0.join("account-reply"),
                format!("echo-install: {verb} completed{newline}"),
            )?;
            connection.account(command).await.map_err(diagnostic)?;
            let calls = fixture.calls()?;
            assert_eq!(
                calls.last(),
                Some(&vec![
                    "-s".into(),
                    "SYNTHETIC-DEVICE".into(),
                    "shell".into(),
                    format!("sh -s -- --device {verb}")
                ])
            );
            assert_eq!(
                fs::read(fixture.0.join("adapter-input"))?,
                echo_setup::DEVICE_SCRIPT.as_bytes()
            );
        }
    }
    assert_eq!(fixture.calls()?.len(), 9);
    Ok(())
}

#[tokio::test]
async fn t3_account_completion_refuses_dependency_failure_and_untrusted_stdout() -> Result {
    let fixture = Fixture::new()?;
    let connection = fixture.connection(deadline()).await?;
    for reply in [
        "",
        "echo-install: signin completed",
        "echo-install: link completed\n",
        "echo-install: signin completed\n\n",
        "echo-install: signin completed\nSYNTHETIC-TOKEN\n",
        "SYNTHETIC-ACCOUNT SYNTHETIC-PRIVATE-PATH\n",
    ] {
        fs::write(fixture.0.join("account-reply"), reply)?;
        let error = connection
            .account(AccountCommand::Signin)
            .await
            .err()
            .ok_or("accepted reply")?;
        assert_eq!(diagnostic(error), "Failure(\"setup_account_failed\")");
        fixture.settle(&connection).await?;
    }
    fs::write(fixture.0.join("account-reply"), "echo-install: signin completed\n")?;
    fs::write(fixture.0.join("account-failed"), "")?;
    assert_eq!(
        connection.account(AccountCommand::Signin).await,
        Err(Failure("setup_account_failed"))
    );
    fs::remove_file(fixture.0.join("account-failed"))?;
    fixture.settle(&connection).await?;
    fs::write(fixture.0.join("operation-failed"), "")?;
    assert_eq!(
        connection.account(AccountCommand::Signin).await,
        Err(Failure("setup_account_failed"))
    );
    Ok(())
}

async fn ready(listener: &TcpListener) -> Result<TcpStream> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (mut socket, _) = listener.accept().await?;
        let mut bytes = [0; 5];
        socket.read_exact(&mut bytes).await?;
        assert_eq!(&bytes, b"ready");
        Ok(socket)
    })
    .await?
}

#[tokio::test]
async fn t3_account_deadline_and_drop_cancel_only_the_local_owned_child() -> Result {
    for expire in [false, true] {
        let fixture = Fixture::new()?;
        let connection = fixture.connection(Instant::now() + Duration::from_secs(5)).await?;
        fs::write(fixture.0.join("account-reply"), "echo-install: signin completed\n")?;
        let listener = fixture.listener("account-barrier")?;
        let mut pending = Box::pin(connection.account(AccountCommand::Signin));
        let mut socket = tokio::select! {
            biased;
            result = &mut pending => return Err(format!("command completed before barrier: {result:?}").into()),
            socket = ready(&listener) => socket?,
        };
        if expire {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(6)).await;
            assert_eq!(pending.await, Err(Failure("setup_timeout")));
            tokio::time::resume();
            let before = fixture.calls()?.len();
            assert_eq!(
                connection.account(AccountCommand::Link).await,
                Err(Failure("setup_timeout"))
            );
            assert_eq!(fixture.calls()?.len(), before);
        } else {
            drop(pending);
        }
        let mut byte = [0];
        let closed = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut byte)).await?;
        assert!(
            matches!(closed, Ok(0)) || closed.is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionReset)
        );
        let calls = fixture.calls()?;
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[3][3], "sh -s -- --device signin");
        fixture.settle(&connection).await?;
        assert_eq!(
            fixture.calls()?.last().map(|args| args[3].as_str()),
            Some("sh -s -- --device stop")
        );
    }
    Ok(())
}

#[tokio::test]
async fn t3_account_cancellation_refuses_overlap_until_verified_stop_on_original_device() -> Result {
    let fixture = Fixture::new()?;
    let connection = fixture.connection(deadline()).await?;
    fs::write(fixture.0.join("account-reply"), "echo-install: signin completed\n")?;
    let listener = fixture.listener("account-barrier")?;
    let mut pending = Box::pin(connection.account(AccountCommand::Signin));
    let mut child = tokio::select! {
        biased;
        result = &mut pending => return Err(format!("command completed before barrier: {result:?}").into()),
        socket = ready(&listener) => socket?,
    };
    let before = fixture.calls()?.len();
    assert_eq!(
        connection.account(AccountCommand::Link).await,
        Err(Failure("setup_account_busy"))
    );
    assert_eq!(
        connection.settle_account(deadline()).await,
        Err(Failure("setup_account_busy"))
    );
    assert_eq!(fixture.calls()?.len(), before);
    drop(pending);
    let mut byte = [0];
    let closed = tokio::time::timeout(Duration::from_secs(5), child.read(&mut byte)).await?;
    assert!(matches!(closed, Ok(0)) || closed.is_err());
    assert_eq!(
        connection.account(AccountCommand::Link).await,
        Err(Failure("setup_account_unsettled"))
    );
    assert_eq!(fixture.calls()?.len(), before);
    fs::write(fixture.0.join("devices"), "SYNTHETIC-OTHER\tdevice\n")?;
    fixture.settle(&connection).await?;
    let calls = fixture.calls()?;
    assert_eq!(calls.len(), before + 1);
    assert_eq!(
        calls.last(),
        Some(&vec![
            "-s".into(),
            "SYNTHETIC-DEVICE".into(),
            "shell".into(),
            "sh -s -- --device stop".into()
        ])
    );
    fs::remove_file(fixture.0.join("account-barrier"))?;
    connection.account(AccountCommand::Signin).await.map_err(diagnostic)?;
    Ok(())
}

#[tokio::test]
async fn t3_account_ambiguous_stop_cannot_authorize_another_owner() -> Result {
    let fixture = Fixture::new()?;
    let connection = fixture.connection(deadline()).await?;
    fs::write(fixture.0.join("account-reply"), "SYNTHETIC-TOKEN")?;
    assert_eq!(
        connection.account(AccountCommand::Signin).await,
        Err(Failure("setup_account_failed"))
    );
    for reply in [
        "",
        "echo-install: stopped",
        "echo-install: stopped\nSYNTHETIC-ACCOUNT\n",
    ] {
        fs::write(fixture.0.join("stop-reply"), reply)?;
        assert_eq!(
            connection.settle_account(deadline()).await,
            Err(Failure("setup_account_unsettled"))
        );
        let before = fixture.calls()?.len();
        assert_eq!(
            connection.account(AccountCommand::Signin).await,
            Err(Failure("setup_account_unsettled"))
        );
        assert_eq!(fixture.calls()?.len(), before);
    }
    fs::write(fixture.0.join("operation-failed"), "")?;
    assert_eq!(
        connection.settle_account(deadline()).await,
        Err(Failure("setup_account_unsettled"))
    );
    fs::remove_file(fixture.0.join("operation-failed"))?;
    fixture.settle(&connection).await?;
    fs::write(fixture.0.join("account-reply"), "echo-install: link completed\n")?;
    connection.account(AccountCommand::Link).await.map_err(diagnostic)?;
    Ok(())
}

#[tokio::test]
async fn t3_account_cancelled_settlement_stays_unsettled_and_expired_cleanup_has_no_effects() -> Result {
    let fixture = Fixture::new()?;
    let connection = fixture.connection(deadline()).await?;
    fs::write(fixture.0.join("account-reply"), "")?;
    assert_eq!(
        connection.account(AccountCommand::Signin).await,
        Err(Failure("setup_account_failed"))
    );
    let before = fixture.calls()?.len();
    assert_eq!(
        connection.settle_account(Instant::now()).await,
        Err(Failure("setup_timeout"))
    );
    assert_eq!(fixture.calls()?.len(), before);
    fs::write(fixture.0.join("stop-reply"), "echo-install: stopped\n")?;
    let listener = fixture.listener("stop-barrier")?;
    let mut pending = Box::pin(connection.settle_account(deadline()));
    let child = tokio::select! {
        biased;
        result = &mut pending => return Err(format!("stop completed before barrier: {result:?}").into()),
        socket = ready(&listener) => socket?,
    };
    assert_eq!(
        connection.account(AccountCommand::Link).await,
        Err(Failure("setup_account_busy"))
    );
    drop(pending);
    drop(child);
    assert_eq!(
        connection.account(AccountCommand::Link).await,
        Err(Failure("setup_account_unsettled"))
    );
    fs::remove_file(fixture.0.join("stop-barrier"))?;
    fixture.settle(&connection).await?;
    Ok(())
}

#[tokio::test]
async fn t3_account_wait_allows_authenticated_prompt_observation_before_completion() -> Result {
    let fixture = Fixture::new()?;
    let connection = fixture.connection(deadline()).await?;
    fs::write(fixture.0.join("account-reply"), "echo-install: signin completed\r\n")?;
    fs::write(fixture.0.join("setup-pairing"), "SYNTHETIC-PAIR\n")?;
    let screen = TcpListener::bind("127.0.0.1:0").await?;
    let replies = [screen.local_addr()?.port().to_string(), "41002".to_owned()];
    fs::write(fixture.0.join("forward-replies.json"), serde_json::to_vec(&replies)?)?;
    fs::write(fixture.0.join("forwards.json"), "[]")?;
    fs::write(fixture.0.join("forward-targets.json"), "{}")?;
    let forwards = connection.forward().await.map_err(diagnostic)?;
    let barrier = fixture.listener("account-barrier")?;
    let mut command = Box::pin(connection.account(AccountCommand::Signin));
    let mut release = tokio::select! {
        biased;
        result = &mut command => return Err(format!("command completed before prompt: {result:?}").into()),
        socket = ready(&barrier) => socket?,
    };
    let server = tokio::spawn(serve_prompt(screen));
    let pair = connection.pairing().await.map_err(diagnostic)?;
    let mut events = forwards.screen(&pair, deadline()).await.map_err(diagnostic)?;
    let prompt = events.next_prompt(Provider::Chatgpt, 100).await.map_err(diagnostic)?;
    let displayed = serde_json::to_value(prompt)?;
    let uri = "https://auth.openai.com/codex/device"; // lint-rules: allow-literal inert expected prompt; HTTP observer only connects to loopback
    assert_eq!(
        displayed,
        serde_json::json!({"type":"prompt", "provider":"chatgpt",
        "user_code":"TEST-CODE", "verification_uri":uri})
    );
    assert!(!displayed.to_string().contains("SYNTHETIC"));
    tokio::select! {
        biased;
        result = &mut command => return Err(format!("screen prompt completed command: {result:?}").into()),
        () = std::future::ready(()) => {}
    }
    release.write_all(&[1]).await?;
    command.await.map_err(diagnostic)?;
    drop(events);
    server.await??;
    forwards.close().await.map_err(diagnostic)?;
    let remaining: Vec<String> = serde_json::from_slice(&fs::read(fixture.0.join("forwards.json"))?)?;
    assert_eq!(remaining, Vec::<String>::new());
    Ok(())
}

async fn serve_prompt(listener: TcpListener) -> Result {
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept()).await??;
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
    let body = "data: {\"seq\":1,\"state\":{\"state\":\"ready\",\"message\":\"code: TEST-CODE\\nurl: https://auth.openai.com/codex/device\\n\"}}\n\n"; // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
    socket
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await?;
    Ok(())
}
