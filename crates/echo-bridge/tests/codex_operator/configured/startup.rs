//! T3 startup admission through the configured owner, real health and OS refusal.
use super::*;
use std::time::Duration;

struct Clock(tokio::task::JoinHandle<()>);
impl Clock {
    fn start() -> Self {
        tokio::time::pause();
        Self(tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        }))
    }
}
impl Drop for Clock {
    fn drop(&mut self) {
        self.0.abort();
        tokio::time::resume();
    }
}

async fn until(path: &Path, event: &str, count: usize) {
    let ceiling = std::time::Instant::now();
    while trace(path).iter().filter(|row| row["event"] == event).count() < count {
        assert!(
            ceiling.elapsed() < Duration::from_secs(8),
            "waiting for {event}: {:?}",
            trace(path)
        );
        tokio::task::yield_now().await;
    }
}

async fn health(path: &Path) -> Value {
    let rows = trace(path);
    let addr = rows.iter().find(|row| row["event"] == "bridge.started").unwrap()["health_addr"]
        .as_str()
        .unwrap()
        .to_owned();
    reqwest::get(format!("http://{addr}/healthz")) // lint-rules: allow-literal address obtained from an owned IPv4 loopback listener
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn count(rows: &[Value], event: &str) -> usize {
    rows.iter().filter(|row| row["event"] == event).count()
}

async fn waiting(path: &Path, attempt: usize) {
    until(path, "bridge.recovering", attempt).await;
    let snapshot = health(path).await;
    assert_eq!(snapshot["relink_required"], false);
    assert_eq!(snapshot["startup_waiting_reason"], "dependency_unavailable");
    assert_eq!(snapshot["wyoming"], "connecting");
    let rows = trace(path);
    let retry = rows
        .iter()
        .filter(|row| row["event"] == "bridge.recovering")
        .nth(attempt - 1)
        .unwrap();
    assert_eq!(retry["attempt"], attempt);
    assert_eq!(retry["delay_ms"], 1000 * (1 << (attempt - 1)));
    assert_eq!(count(&rows, "bridge.ready"), 0);
    assert_eq!(count(&rows, "auth.relink_required"), 0);
}

async fn ready_health(path: &Path) {
    until(path, "bridge.ready", 1).await;
    let state = health(path).await;
    assert_eq!(state["relink_required"], false);
    assert!(state.get("startup_waiting_reason").is_none());
}

async fn relink(path: &Path) {
    until(path, "auth.relink_required", 1).await;
    let state = health(path).await;
    assert_eq!(state["relink_required"], true);
    assert!(state.get("startup_waiting_reason").is_none());
    let rows = trace(path);
    let address = rows.iter().find(|row| row["event"] == "screen.listening").unwrap()["address"]
        .as_str()
        .unwrap();
    let mut screen = bridge_wire_screen::stream(address).await;
    assert_eq!(
        bridge_wire_screen::state(&mut screen).await,
        json!({"state":"error", "relink":true,
            "message":"Run echo-bridge signin with your config, then restart the bridge."})
    );
}

async fn exercise(mode: &str, inspect: impl AsyncFnOnce(&Fixture, &Path, &Observer)) -> Vec<Value> {
    let clock = Clock::start();
    let fixture = Fixture::new("chatgpt");
    std::fs::write(fixture.home.join("mode"), mode).unwrap();
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    std::fs::write(
        fixture.home.join("provider-port"),
        reserved.local_addr().unwrap().port().to_string(),
    )
    .unwrap();
    drop(reserved);
    let auth = b"synthetic unchanged startup credentials";
    std::fs::write(fixture.home.join("auth.json"), auth).unwrap();
    let path = fixture.directory.path().join("startup-trace");
    let mut settings = config(&fixture);
    settings.mcp.server.clear();
    let prepared = Configured::new(settings, path.clone(), ([127, 0, 0, 1], 0).into()).unwrap();
    let observer = Observer::default();
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let work = prepared.execute(
        FakeConnector::new(observer.clone()),
        async || -> Result<echo_media::Peer, Error> { unreachable!("no wake, no media") },
        vec![],
        async {
            stopping.await.unwrap();
        },
    );
    let inspect = async {
        until(&path, "bridge.started", 1).await;
        inspect(&fixture, &path, &observer).await;
        stop.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(work, inspect);
    assert_eq!(
        result,
        if count(&trace(&path), "auth.relink_required") == 1 {
            Err(Error::Authorization)
        } else {
            Ok(())
        }
    );
    cleanup(&fixture, &path, auth).await;
    drop(clock);
    trace(&path)
}

async fn cleanup(fixture: &Fixture, path: &Path, auth: &[u8]) {
    super::super::host::released(fixture).await;
    assert_eq!(std::fs::read(fixture.home.join("auth.json")).unwrap(), auth);
    presentation_released(path, "chatgpt", true);
    assert!(tokio::net::TcpStream::connect(fixture.screen).await.is_err());
    let address = trace(path)
        .into_iter()
        .find(|row| row["event"] == "bridge.started")
        .unwrap();
    assert!(
        tokio::net::TcpStream::connect(address["health_addr"].as_str().unwrap())
            .await
            .is_err()
    );
}

#[tokio::test]
#[ignore = "T3 local Python and refused loopback provider"]
async fn startup_network_two_refusals_then_ready_without_relink_or_restart() {
    let rows = exercise("startup-network", async |fixture, path, observer| {
        waiting(path, 1).await;
        assert!(observer.commands().is_empty());
        tokio::time::advance(Duration::from_secs(1)).await;
        waiting(path, 2).await;
        assert!(observer.commands().is_empty());
        let port: u16 = std::fs::read_to_string(fixture.home.join("provider-port"))
            .unwrap()
            .parse()
            .unwrap();
        let provider = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        let _accepted = provider.accept().await.unwrap();
        ready_health(path).await;
        assert_eq!(
            std::fs::read_to_string(fixture.home.join("provider-attempts")).unwrap(),
            "refused\nrefused\naccepted\n"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.home.join("launches"))
                .unwrap()
                .lines()
                .count(),
            3
        );
    })
    .await;
    assert_eq!(count(&rows, "bridge.recovering"), 2);
    assert_eq!(count(&rows, "auth.relink_required"), 0);
    assert_eq!(count(&rows, "bridge.ready"), 1);
}

#[tokio::test]
#[ignore = "T3 local controlled process"]
async fn startup_network_timeout_retries_and_auth_transition_clears_waiting() {
    for reject in [false, true] {
        let rows = exercise("startup-timeout", async |fixture, path, observer| {
            waiting(path, 1).await;
            assert!(observer.commands().is_empty());
            std::fs::write(
                fixture.home.join("mode"),
                if reject { "startup-unauthorized" } else { "chatgpt" },
            )
            .unwrap();
            tokio::time::advance(Duration::from_secs(1)).await;
            if reject {
                relink(path).await;
            } else {
                ready_health(path).await;
            }
        })
        .await;
        assert_eq!(count(&rows, "bridge.recovering"), 1);
        assert_eq!(count(&rows, "bridge.ready"), usize::from(!reject));
        assert_eq!(count(&rows, "auth.relink_required"), usize::from(reject));
    }
}

#[tokio::test]
#[ignore = "T3 local controlled process"]
async fn startup_network_healthy_and_definite_auth_keep_existing_admission() {
    for mode in [
        "chatgpt",
        "startup-unauthorized",
        "rejected-account",
        "expired-account",
        "signed-out",
        "api_key",
    ] {
        let happy = mode == "chatgpt";
        let rows = exercise(mode, async |_, path, observer| {
            if happy {
                ready_health(path).await;
            } else {
                relink(path).await;
                assert!(observer.commands().is_empty());
            }
        })
        .await;
        assert_eq!(count(&rows, "bridge.ready"), usize::from(happy));
        assert_eq!(count(&rows, "auth.relink_required"), usize::from(!happy));
        assert_eq!(count(&rows, "bridge.recovering"), 0);
    }
}

#[tokio::test]
#[ignore = "T3 local controlled process"]
async fn startup_network_cancel_during_account_or_retry_releases_all_owners() {
    for blocked in [false, true] {
        let rows = exercise(
            if blocked { "blocked-account" } else { "startup-timeout" },
            async |fixture, path, observer| {
                if blocked {
                    let _control = fixture.control().await;
                    while !std::fs::read_to_string(fixture.home.join("last-method")).is_ok_and(|m| m == "account/read")
                    {
                        tokio::task::yield_now().await;
                    }
                } else {
                    waiting(path, 1).await;
                }
                assert!(observer.commands().is_empty());
            },
        )
        .await;
        assert_eq!(count(&rows, "bridge.ready"), 0);
        assert_eq!(count(&rows, "bridge.recovering"), usize::from(!blocked));
    }
}

#[tokio::test]
#[ignore = "T3 local controlled process and virtual startup deadline"]
async fn startup_network_stalled_account_deadline_retries_to_ready() {
    let rows = exercise("blocked-account", async |fixture, path, observer| {
        let control = fixture.control().await;
        while !std::fs::read_to_string(fixture.home.join("last-method")).is_ok_and(|m| m == "account/read") {
            tokio::task::yield_now().await;
        }
        assert!(observer.commands().is_empty());
        tokio::time::advance(Duration::from_secs(30)).await;
        waiting(path, 1).await;
        std::fs::write(fixture.home.join("mode"), "chatgpt").unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        ready_health(path).await;
        drop(control);
    })
    .await;
    assert_eq!(count(&rows, "bridge.recovering"), 1);
    assert_eq!(count(&rows, "bridge.ready"), 1);
}
