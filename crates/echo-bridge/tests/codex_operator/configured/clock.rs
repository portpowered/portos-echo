//! T3: a system clock that is hours behind; fake SNTP servers on 127.0.0.1 hold
//! the right time. The configured owner measures before it admits the device,
//! traces the offset and tells both models the corrected call-start time.
use super::super::tools::{Media, ready};
use super::*;
use echo_media::PeerState;
use std::sync::{Arc, OnceLock, atomic::Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

/// 17.5 h: what the Echo was behind by when this was found.
const BEHIND_SECONDS: i64 = 63_000;
const NTP_UNIX_SECONDS: i64 = 2_208_988_800;

#[allow(clippy::disallowed_methods, reason = "Host time for test")]
fn system_seconds() -> i64 {
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()).unwrap()
}

/// An SNTP server whose clock is `BEHIND_SECONDS` ahead of this machine's.
async fn time_server() -> String {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let name = socket.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let mut request = [0; 48];
        while let Ok((48, peer)) = socket.recv_from(&mut request).await {
            let seconds = u64::try_from(system_seconds() + BEHIND_SECONDS + NTP_UNIX_SECONDS).unwrap();
            let mut reply = [0; 48];
            (reply[0], reply[1]) = (0x24, 2);
            reply[24..32].copy_from_slice(&request[40..48]);
            reply[32..40].copy_from_slice(&(seconds << 32).to_be_bytes());
            reply[40..48].copy_from_slice(&(seconds << 32).to_be_bytes());
            socket.send_to(&reply, peer).await.unwrap();
        }
    });
    name
}

/// Unix seconds of `YYYY-MM-DD Weekday HH:MM:SS`, by the days-from-civil rule.
fn call_start_seconds(context: &str) -> i64 {
    let mut parts = context.split(' ');
    let date: Vec<i64> = parts.next().unwrap().split('-').map(|n| n.parse().unwrap()).collect();
    let time: Vec<i64> = parts.nth(1).unwrap().split(':').map(|n| n.parse().unwrap()).collect();
    let (year, month, day) = (date[0] - i64::from(date[1] <= 2), date[1], date[2]);
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    (era * 146_097 + day_of_era - 719_468) * 86_400 + time[0] * 3600 + time[1] * 60 + time[2]
}

#[test]
fn call_start_seconds_reads_the_bridge_calendar() {
    let epoch = "1970-01-01 Thursday 00:00:00 UTC+00:00"; // lint-rules: allow-literal UTC calendar expectation, never an endpoint
    let later = "2026-11-17 Tuesday 12:34:56 UTC+00:00"; // lint-rules: allow-literal UTC calendar expectation, never an endpoint
    assert_eq!(call_start_seconds(epoch), 0);
    assert_eq!(call_start_seconds(later), 1_794_918_896);
}

#[tokio::test]
#[ignore = "T3 requires local Python and loopback MCP/SNTP"]
async fn t3_clock_configured_call_tells_the_models_the_measured_time() {
    let fixture = Fixture::new("chatgpt");
    std::fs::write(fixture.home.join("mode"), "tool-reply").unwrap();
    let spoof = echo_spoof_mcp::Spoof::start(echo_spoof_mcp::Options::default())
        .await
        .unwrap();
    let mut config = super::super::tools::config(&fixture, spoof.url());
    config.clock.servers = Some(vec![time_server().await, time_server().await, time_server().await]);
    let path = fixture.directory.path().join("clock-trace");
    let owner = Configured::new(config, path.clone(), ([127, 0, 0, 1], 0).into()).unwrap();
    let observer = Observer::default();
    let closed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let work = owner.execute(
        FakeConnector::new(observer.clone()),
        async || {
            Ok(Media {
                state: watch::channel(PeerState::New).0,
                closed: closed.clone(),
                setup: OnceLock::new(),
                lost: Arc::default(),
            })
        },
        vec![],
        async { stopping.await.unwrap() },
    );
    let inspect = async {
        corrected_call(&fixture, &observer, &path).await;
        stop.send(()).unwrap();
    };
    let (result, ()) = Box::pin(tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        tokio::join!(work, inspect)
    }))
    .await
    .unwrap();
    assert_eq!(result, Ok(()));
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    super::super::host::released(&fixture).await;
    spoof.shutdown().await.unwrap();
}

async fn corrected_call(fixture: &Fixture, observer: &Observer, path: &Path) {
    let (_control, _screen) = ready(fixture).await;
    let before = system_seconds();
    observer.wake("clock fixture", vec![]);
    while !observer.commands().contains(&Command::Stop) {
        assert!(trace(path).iter().all(|row| row["event"] != "bridge.recovering"));
        tokio::task::yield_now().await;
    }
    let after = system_seconds();
    for (file, field) in [("thread-config", "baseInstructions"), ("realtime-config", "prompt")] {
        let config: Value = serde_json::from_slice(&std::fs::read(fixture.home.join(file)).unwrap()).unwrap();
        let (_, context) = config[field]
            .as_str()
            .unwrap()
            .split_once("[Call-start context: ")
            .unwrap();
        let told = call_start_seconds(context);
        assert!(
            (before + BEHIND_SECONDS - 2..=after + BEHIND_SECONDS + 2).contains(&told),
            "{file}: told {told}, system {before}..{after}"
        );
    }
    let rows = trace(path);
    let measured: Vec<_> = rows.iter().filter(|row| row["event"] == "clock.offset").collect();
    assert_eq!(measured.len(), 1);
    let offset = measured[0]["offset_ms"].as_i64().unwrap();
    assert!((offset - BEHIND_SECONDS * 1000).abs() < 2000, "{offset}");
    assert_eq!(
        (
            &measured[0]["synced"],
            &measured[0]["hosts"],
            &measured[0]["servers"],
            &measured[0]["corrected"]
        ),
        (&json!(true), &json!(3), &json!(3), &json!(true))
    );
    let position = |event| rows.iter().position(|row| row["event"] == event).unwrap();
    assert!(position("clock.offset") < position("bridge.ready"));
    observer.finish(Ok(()));
    while trace(path).iter().all(|row| row["event"] != "bridge.run_completed") {
        tokio::task::yield_now().await;
    }
}
