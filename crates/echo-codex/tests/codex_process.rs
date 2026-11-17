//! T3 controlled local stdio processes; no network or real account.
use echo_codex::{
    codex_protocol::Message,
    codex_rpc::RpcError,
    codex_supervisor::{ProcessError, Supervisor, SupervisorConfig},
};
use serde_json::{Value, json};
use std::{
    error::Error,
    fs,
    io::BufRead,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, BufReader};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    supervisor: Option<Supervisor>,
    home: PathBuf,
}
impl Fixture {
    fn new(mode: &str) -> Result<Self> {
        let output = Command::new(if cfg!(windows) { "python" } else { "python3" })
            .args(["-c", "import sys; print(sys.executable)"])
            .output()?;
        assert!(output.status.success());
        let binary = PathBuf::from(String::from_utf8(output.stdout)?.trim());
        let home = std::env::temp_dir().join(format!(
            "echo codex process-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut config = SupervisorConfig::new(binary, home.clone());
        config.request_timeout = Duration::from_secs(5);
        config.max_line_bytes = 4096;
        config
            .config_overrides
            .push(("test.argument".into(), "\"space ; $literal\"".into()));
        let supervisor = Supervisor::new(config)?;
        fs::write(
            home.join("work/app-server"),
            include_str!("support/codex_stdio_peer.py"),
        )?;
        fs::write(home.join("work/mode"), mode)?;
        Ok(Self {
            supervisor: Some(supervisor),
            home,
        })
    }
    fn server(&mut self) -> Result<&mut Supervisor> {
        self.supervisor.as_mut().ok_or_else(|| "fixture already dropped".into())
    }
    async fn inspect(&mut self, method: &str) -> Result<Value> {
        let io = self.server()?.io()?;
        let id = io.begin_request(method, &json!({}))?;
        Ok(io.wait_reply(&id).await?)
    }
    fn hold_initialize(&self) -> Result<tokio::task::JoinHandle<std::io::Result<String>>> {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        fs::write(
            self.home.join("work/mode"),
            format!("hold-initialize:{}", listener.local_addr()?.port()),
        )?;
        Ok(tokio::task::spawn_blocking(move || {
            let (stream, _) = listener.accept()?;
            let mut signal = String::new();
            std::io::BufReader::new(stream).read_line(&mut signal)?;
            Ok(signal)
        }))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.supervisor.take();
        let _ = fs::remove_dir_all(&self.home);
    }
}

#[tokio::test]
async fn t3_handshake_private_environment_argv_and_shutdown() -> Result {
    let mut fixture = Fixture::new("normal")?;
    assert!(matches!(fixture.server()?.io(), Err(ProcessError::Unavailable)));
    assert_eq!(
        fixture
            .server()?
            .set_environment("OPENAI_API_KEY", "dummy-local-only".into()),
        Err(ProcessError::InvalidConfig)
    );
    fixture.server()?.set_environment("NO_PROXY", "127.0.0.1".into())?;
    fixture.server()?.start().await?;
    let result = fixture.inspect("stderr").await?;
    assert_eq!(result["initialized"], true);
    assert_eq!(
        PathBuf::from(result["home"].as_str().ok_or("expected text")?),
        fixture.home
    );
    assert_eq!(
        PathBuf::from(result["cwd"].as_str().ok_or("expected text")?),
        fixture.home.join("work")
    );
    assert_eq!(
        result["argv"],
        json!(["--listen", "stdio://", "-c", "test.argument=\"space ; $literal\""])
    );
    let allowed = [
        "SYSTEMROOT",
        "WINDIR",
        "PATH",
        "TEMP",
        "TMP",
        "COMSPEC",
        "HOME",
        "USERPROFILE",
        "CODEX_HOME",
        "NO_PROXY",
        "LC_CTYPE",
    ];
    for key in result["environment"].as_array().ok_or("expected array")? {
        assert!(
            allowed.contains(&key.as_str().ok_or("expected text")?),
            "unexpected inherited key: {key}"
        );
    }
    assert_eq!(fixture.server()?.start().await, Err(ProcessError::Unavailable));
    fixture.server()?.shutdown().await?;
    assert_eq!(fixture.server()?.pid(), None);
    fixture.server()?.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn t3_refused_handshake_and_eof_never_expose_io() -> Result {
    for (mode, expected) in [("refuse", RpcError::Remote(-32600)), ("eof", RpcError::Disconnected)] {
        let mut fixture = Fixture::new(mode)?;
        assert_eq!(fixture.server()?.start().await, Err(ProcessError::Rpc(expected)));
        assert_eq!(fixture.server()?.pid(), None);
        assert!(matches!(fixture.server()?.io(), Err(ProcessError::Unavailable)));
    }
    Ok(())
}

#[tokio::test]
async fn t3_cancelled_initialize_keeps_child_private_until_restart() -> Result {
    let mut fixture = Fixture::new("normal")?;
    let mut barrier = fixture.hold_initialize()?;
    {
        let start = fixture.server()?.start();
        tokio::pin!(start);
        tokio::select! {
            result = &mut start => return Err(format!("unexpected initialize completion: {result:?}").into()),
            signal = &mut barrier => assert_eq!(signal??, "INITIALIZE_RECEIVED\n"),
        }
        // Dropping this future must preserve ownership without publishing IO.
    }
    let old = fixture.server()?.pid().ok_or("expected initializing child")?;
    assert!(matches!(fixture.server()?.io(), Err(ProcessError::Unavailable)));
    assert_eq!(fixture.server()?.start().await, Err(ProcessError::Unavailable));
    let (mut observer, mut lines) = observe_exit(old).await?;
    fs::write(fixture.home.join("work/mode"), "normal")?;
    fixture.server()?.restart().await?;
    assert_eq!(lines.next_line().await?.as_deref(), Some("EXITED"));
    assert!(observer.wait().await?.success());
    assert_ne!(fixture.server()?.pid(), Some(old));
    assert_eq!(fixture.inspect("inspect").await?["initialized"], true);
    fixture.server()?.shutdown().await?;
    assert_eq!(fixture.server()?.pid(), None);
    Ok(())
}

#[tokio::test]
async fn t3_stalled_initialize_expires_and_reaps_without_publishing_io() -> Result {
    let mut fixture = Fixture::new("normal")?;
    let mut barrier = fixture.hold_initialize()?;
    let outcome;
    {
        let start = fixture.server()?.start();
        tokio::pin!(start);
        tokio::select! {
            result = &mut start => return Err(format!("unexpected initialize completion: {result:?}").into()),
            signal = &mut barrier => assert_eq!(signal??, "INITIALIZE_RECEIVED\n"),
        }
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(6)).await;
        // OS child reaping uses real readiness, so do not auto-advance its watchdog.
        tokio::time::resume();
        outcome = start.await;
    }
    assert_eq!(outcome, Err(ProcessError::Rpc(RpcError::Deadline)));
    assert_eq!(fixture.server()?.pid(), None);
    assert!(matches!(fixture.server()?.io(), Err(ProcessError::Unavailable)));
    fs::write(fixture.home.join("work/mode"), "normal")?;
    fixture.server()?.restart().await?;
    assert_eq!(fixture.inspect("inspect").await?["initialized"], true);
    fixture.server()?.shutdown().await?;
    Ok(())
}

async fn observe_exit(
    pid: u32,
) -> Result<(
    tokio::process::Child,
    tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
)> {
    let mut observer = tokio::process::Command::new(if cfg!(windows) { "python" } else { "python3" })
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/codex_watch.py"))
        .arg(pid.to_string())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = observer.stdout.take().ok_or("expected observer pipe")?;
    let mut lines = BufReader::new(stdout).lines();
    assert_eq!(lines.next_line().await?.as_deref(), Some("WATCHING"));
    Ok((observer, lines))
}

#[tokio::test]
async fn t3_crash_restart_reinitializes_without_replaying_pending_work() -> Result {
    let mut fixture = Fixture::new("normal")?;
    fixture.server()?.start().await?;
    let mut old = Vec::new();
    for _ in 0..2 {
        let pid = fixture.server()?.pid().ok_or("expected live PID")?;
        assert!(!old.contains(&pid));
        old.push(pid);
        let io = fixture.server()?.io()?;
        let pending = io.begin_request("hold", &json!({}))?;
        assert!(matches!(io.next_event().await?, Message::Notification {method, ..} if method == "holding"));
        let crash = io.begin_request("crash", &json!({}))?;
        assert_eq!(io.wait_reply(&crash).await, Err(RpcError::Disconnected));
        assert_eq!(io.wait_reply(&pending).await, Err(RpcError::Disconnected));
        assert_eq!(io.wait_reply(&pending).await, Err(RpcError::UnknownRequest));
        fixture.server()?.restart().await?;
        assert_eq!(fixture.inspect("inspect").await?["initialized"], true);
    }
    assert_eq!(fixture.server()?.restart().await, Err(ProcessError::Unavailable));
    assert_eq!(fixture.server()?.pid(), None);
    Ok(())
}

#[tokio::test]
async fn t3_protocol_fault_closes_pipes_and_explicit_shutdown_reaps_child() -> Result {
    for (method, expected) in [("malformed", RpcError::Malformed), ("oversize", RpcError::LineTooLong)] {
        let mut fixture = Fixture::new("normal")?;
        fixture.server()?.start().await?;
        let io = fixture.server()?.io()?;
        let id = io.begin_request(method, &json!({}))?;
        assert_eq!(io.wait_reply(&id).await, Err(expected));
        fixture.server()?.shutdown().await?;
        assert_eq!(fixture.server()?.pid(), None);
    }
    Ok(())
}

#[tokio::test]
async fn t3_held_process_request_expires_once_on_the_client_clock() -> Result {
    let mut fixture = Fixture::new("normal")?;
    fixture.server()?.start().await?;
    let io = fixture.server()?.io()?;
    let id = io.begin_request("hold", &json!({}))?;
    assert!(matches!(io.next_event().await?, Message::Notification {method, ..} if method == "holding"));
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(6)).await;
    let reply = io.wait_reply(&id).await;
    tokio::time::resume();
    assert_eq!(reply, Err(RpcError::Deadline));
    assert_eq!(io.wait_reply(&id).await, Err(RpcError::UnknownRequest));
    fixture.server()?.shutdown().await?;
    assert_eq!(fixture.server()?.pid(), None);
    Ok(())
}

#[tokio::test]
async fn t3_sessions_are_independent_across_restart_and_cleanup() -> Result {
    let (mut first, mut second) = (Fixture::new("normal")?, Fixture::new("normal")?);
    let (left, right) = tokio::join!(first.server()?.start(), second.server()?.start());
    left?;
    right?;
    assert_ne!(first.server()?.pid(), second.server()?.pid());
    assert_ne!(first.home, second.home);
    first.server()?.shutdown().await?;
    assert_eq!(second.inspect("inspect").await?["initialized"], true);
    second.server()?.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn t3_drop_kills_active_child_releases_port_and_preserves_other_session() -> Result {
    let (mut first, mut second) = (Fixture::new("normal")?, Fixture::new("normal")?);
    let (left, right) = tokio::join!(first.server()?.start(), second.server()?.start());
    left?;
    right?;
    let port = first.inspect("listen").await?["port"].as_u64().ok_or("expected port")?;
    let pid = first.server()?.pid().ok_or("expected child")?;
    let io = first.server()?.io()?;
    io.begin_request("hold", &json!({}))?;
    assert!(matches!(io.next_event().await?, Message::Notification {method, ..} if method == "holding"));
    let (mut observer, mut lines) = observe_exit(pid).await?;
    drop(first.supervisor.take());
    assert_eq!(lines.next_line().await?.as_deref(), Some("EXITED"));
    assert!(observer.wait().await?.success());
    let rebound = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, u16::try_from(port)?))?;
    assert_eq!(u64::from(rebound.local_addr()?.port()), port);
    assert_eq!(second.inspect("inspect").await?["initialized"], true);
    second.server()?.shutdown().await?;
    Ok(())
}

#[test]
fn t3_rejects_existing_repository_and_relative_homes_before_spawn() -> Result {
    let fixture = Fixture::new("normal")?;
    let binary = std::env::current_exe()?;
    for home in [
        fixture.home.clone(),
        PathBuf::from("relative-home"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("uncreated-home"),
    ] {
        assert!(matches!(
            Supervisor::new(SupervisorConfig::new(binary.clone(), home)),
            Err(ProcessError::PrivateHome)
        ));
    }
    Ok(())
}
