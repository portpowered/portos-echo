//! T3: native recording adb and isolated files; no device/account/provider.
//! The shell's actual log safety is separately exercised by `test_echo_install.py`.
use echo_setup::{
    Failure,
    cli::Options,
    setup::{WakeAttempt, WakeEvidence},
};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::Instant,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new(cursor: &str) -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "echo-wake-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        fs::copy(env!("CARGO_BIN_EXE_echo-setup-test-adb"), path.join("adb.exe"))?;
        fs::write(path.join("devices"), "SYNTHETIC-DEVICE\tdevice\r\n")?;
        fs::write(path.join("wake-cursor"), cursor)?;
        Ok(Self(path))
    }

    fn options(&self) -> Result<Options> {
        Options::parse(["status".into(), "--adb".into(), self.0.join("adb.exe").into_os_string()])
            .map_err(|error| format!("{error:?}").into())
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

fn cursor() -> String {
    format!("1:2:42:{}\r\n", "a".repeat(64))
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

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(20)
}

#[tokio::test]
async fn t3_wake_attempt_uses_one_cursor_and_requires_ordered_fresh_link_evidence() -> Result {
    let fixture = Fixture::new(&cursor())?;
    let attempt = WakeAttempt::begin(&fixture.options()?, deadline())
        .await
        .map_err(fixed)?;
    for (frame, expected) in [
        (
            "wake=pending\r\nlink=pending\r\n",
            WakeEvidence {
                detected: false,
                linked: false,
            },
        ),
        (
            "wake=confirmed\nlink=pending\n",
            WakeEvidence {
                detected: true,
                linked: false,
            },
        ),
        (
            "wake=confirmed\r\nlink=connected\r\n",
            WakeEvidence {
                detected: true,
                linked: true,
            },
        ),
    ] {
        fs::write(fixture.0.join("wake-evidence"), frame)?;
        assert_eq!(attempt.observe().await.map_err(fixed)?, expected);
    }
    assert_eq!(
        attempt.wait_for_link().await.map_err(fixed)?,
        WakeEvidence {
            detected: true,
            linked: true
        }
    );
    let calls = fixture.calls()?;
    assert_eq!(calls.len(), 7);
    assert_eq!(calls[0], ["devices"]);
    assert_eq!(
        calls[1],
        ["-s", "SYNTHETIC-DEVICE", "shell", "test -x /data/local/hassmic/runas"]
    );
    assert_eq!(calls[2][3], "sh -s -- --device wake-cursor");
    for args in &calls[3..] {
        assert_eq!(args[..3], ["-s", "SYNTHETIC-DEVICE", "shell"]);
        assert_eq!(args[3], format!("sh -s -- --device wake-check '{}'", cursor().trim()));
    }
    assert_eq!(
        fs::read_to_string(fixture.0.join("adapter-input"))?,
        echo_setup::DEVICE_SCRIPT
    );
    Ok(())
}

#[tokio::test]
async fn t3_wake_refuses_invalid_cursor_and_failed_or_private_observation_without_rebaseline() -> Result {
    for bad in ["SYNTHETIC-TOKEN\n", "1:2:3:';id\n", "", "1:2:3:abc\n"] {
        let fixture = Fixture::new(bad)?;
        assert_eq!(
            WakeAttempt::begin(&fixture.options()?, deadline()).await.err(),
            Some(Failure("setup_wake_unavailable"))
        );
        assert_eq!(fixture.calls()?.len(), 3);
    }
    let fixture = Fixture::new(&cursor())?;
    let attempt = WakeAttempt::begin(&fixture.options()?, deadline())
        .await
        .map_err(fixed)?;
    fs::write(
        fixture.0.join("wake-evidence"),
        "wake=pending\nlink=connected\nSYNTHETIC-ACCOUNT\n",
    )?;
    assert_eq!(
        attempt.observe().await.map_err(fixed),
        Err(fixed(Failure("setup_wake_unavailable")))
    );
    fs::write(fixture.0.join("operation-failed"), "")?;
    assert_eq!(attempt.wait_for_link().await, Err(Failure("setup_wake_unavailable")));
    assert_eq!(
        fixture
            .calls()?
            .iter()
            .filter(|args| args.last().is_some_and(|arg| arg.ends_with("wake-cursor")))
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn t3_wake_expired_deadline_has_no_effect_and_pending_attempt_never_completes() -> Result {
    let fixture = Fixture::new(&cursor())?;
    assert_eq!(
        WakeAttempt::begin(&fixture.options()?, Instant::now()).await.err(),
        Some(Failure("setup_timeout"))
    );
    assert!(!fixture.0.join("ledger").exists());
    let until = deadline();
    let attempt = WakeAttempt::begin(&fixture.options()?, until).await.map_err(fixed)?;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(21)).await;
    assert_eq!(attempt.wait_for_link().await, Err(Failure("setup_timeout")));
    assert_eq!(fixture.calls()?.len(), 3);
    Ok(())
}

#[tokio::test]
async fn t3_wake_deadline_cancels_owned_adb_child_without_reporting_success() -> Result {
    let fixture = Fixture::new(&cursor())?;
    let attempt = WakeAttempt::begin(&fixture.options()?, deadline())
        .await
        .map_err(fixed)?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    fs::write(
        fixture.0.join("wake-barrier"),
        listener.local_addr()?.port().to_string(),
    )?;
    fs::write(fixture.0.join("wake-evidence"), "wake=confirmed\nlink=connected\n")?;
    let waiting = tokio::spawn(async move { attempt.wait_for_link().await });
    let (mut peer, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept()).await??;
    let mut ready = [0; 5];
    tokio::time::timeout(Duration::from_secs(10), peer.read_exact(&mut ready)).await??;
    assert_eq!(&ready, b"ready");
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(21)).await;
    assert_eq!(waiting.await?, Err(Failure("setup_timeout")));
    tokio::time::resume();
    let mut byte = [0];
    let closed = tokio::time::timeout(Duration::from_secs(5), peer.read(&mut byte)).await?;
    assert!(matches!(closed, Ok(0)) || closed.is_err());
    peer.shutdown().await?;
    assert_eq!(fixture.calls()?.len(), 4);
    Ok(())
}
