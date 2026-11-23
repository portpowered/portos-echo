//! T0 correction and operator-command rules; T3 startup measurement against 127.0.0.1.
use super::sntp::Outcome;
use super::sntp_tests::{BEHIND_MS, Fake};
use super::{clock_command, corrected, now, start};
use crate::{Error, bridge_trace::Trace};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn at(unix_ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(unix_ms)
}

#[test]
fn time_uses_the_measured_time_only_while_the_system_clock_is_off_by_seconds() {
    let system = at(1_000_000_000);
    assert_eq!(corrected(system, None), system);
    // Network time 17.5 h ahead of, then behind, the system clock.
    assert_eq!(corrected(system, Some(1_063_000_000)), at(1_063_000_000));
    assert_eq!(corrected(system, Some(937_000_000)), at(937_000_000));
    assert_eq!(corrected(system, Some(1_000_002_001)), at(1_000_002_001));
    // Within two seconds the system clock is right, also once root has set it mid-run.
    for reference in [1_000_002_000, 999_998_000, 1_000_000_000] {
        assert_eq!(corrected(system, Some(reference)), system);
    }
    assert_eq!(corrected(system, Some(-5)), system);
}

fn rows(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn ahead_of_system(time: SystemTime) -> i64 {
    let system = super::sntp::system_ms();
    i64::try_from(time.duration_since(UNIX_EPOCH).unwrap().as_millis()).unwrap() - system
}

#[tokio::test]
async fn t3_time_startup_measures_traces_and_corrects_a_clock_that_is_behind() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let mut servers = vec![];
    for _ in 0..3 {
        servers.push(Fake::ahead(BEHIND_MS).serve().await);
    }
    assert!(ahead_of_system(now()).abs() < 250);
    let watch = start(servers, Duration::from_secs(3600), Trace::open(&path, "chatgpt").ok()).await;
    assert!((ahead_of_system(now()) - BEHIND_MS).abs() < 250);
    let rows = rows(&path);
    assert_eq!(rows.len(), 1);
    let offset = rows[0]["offset_ms"].as_i64().unwrap();
    assert!((offset - BEHIND_MS).abs() < 250, "{offset}");
    let mut row = rows[0].clone();
    row.as_object_mut().unwrap().remove("elapsed_ms");
    assert_eq!(
        row,
        json!({"event":"clock.offset", "provider_auth_mode":"chatgpt", "synced":true,
            "offset_ms":offset, "hosts":3, "servers":3, "corrected":true})
    );
    drop(watch);
}

#[tokio::test]
async fn t3_time_unreachable_servers_change_nothing_and_say_unsynced_once() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let silent = socket.local_addr().unwrap().to_string();
    let _watch = start(
        vec![silent],
        Duration::from_secs(3600),
        Trace::open(&path, "api_key").ok(),
    )
    .await;
    assert!(ahead_of_system(now()).abs() < 250);
    let mut rows = rows(&path);
    assert_eq!(rows.len(), 1);
    rows[0].as_object_mut().unwrap().remove("elapsed_ms");
    assert_eq!(
        rows[0],
        json!({"event":"clock.offset", "provider_auth_mode":"api_key", "synced":false,
            "reason":"unreachable", "hosts":0, "servers":1, "corrected":false})
    );
}

#[tokio::test]
async fn t3_time_no_servers_measure_and_trace_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let _watch = start(vec![], Duration::from_secs(3600), Trace::open(&path, "chatgpt").ok()).await;
    assert_eq!(rows(&path), Vec::<Value>::new());
}

async fn invalid_sntp_reply() -> Outcome {
    use echo_core_api::{Clock, SystemClock};

    // The clock monitor receives an invalid SNTP reply on each loopback attempt.
    let mut fake = Fake::ahead(0);
    fake.stratum = 0;
    let server = fake.serve().await;
    let query = tokio::spawn(async move { super::sntp::measure(&[server], Duration::from_millis(400)).await });
    // Let the real loopback socket progress without allowing Tokio to auto-advance.
    let watchdog = SystemClock.now().monotonic + Duration::from_secs(2);
    while !query.is_finished() {
        assert!(
            SystemClock.now().monotonic < watchdog,
            "loopback clock query did not finish"
        );
        tokio::task::yield_now().await;
    }
    query.await.unwrap()
}

#[tokio::test(start_paused = true)]
async fn t3_time_failure_diagnostic_drains_without_a_followup_event() {
    use echo_core_api::{Clock, SystemClock};

    let outcome = invalid_sntp_reply().await;
    assert!(matches!(
        outcome,
        Outcome::Unsynced {
            reason: "unreachable",
            answered: 0
        }
    ));

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let trace = Trace::open(&path, "api_key").unwrap();
    let mut reporter = super::Reporter {
        trace: Some(trace),
        servers: 1,
        unsynced: false,
    };
    assert!(!reporter.observe(outcome, false));

    tokio::task::yield_now().await;
    assert_eq!(std::fs::read(&path).unwrap(), b"");
    let buffered_at = tokio::time::Instant::now();
    tokio::time::advance(echo_sub_trace::TICK).await;
    let watchdog = SystemClock.now().monotonic + Duration::from_secs(2);
    let bytes = loop {
        let bytes = std::fs::read(&path).unwrap();
        if bytes.ends_with(b"\n") {
            break bytes;
        }
        assert!(
            SystemClock.now().monotonic < watchdog,
            "clock failure exceeded one trace tick"
        );
        tokio::task::yield_now().await;
    };
    assert_eq!(buffered_at.elapsed(), echo_sub_trace::TICK);
    let mut row: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(row.as_object_mut().unwrap().remove("corrected").unwrap().is_boolean());
    assert_eq!(
        row,
        json!({"elapsed_ms":0,"event":"clock.offset","provider_auth_mode":"api_key",
        "synced":false,"reason":"unreachable","servers":1,"hosts":0})
    );
}

fn args(text: &str) -> Vec<String> {
    text.split_whitespace().map(String::from).collect()
}

#[test]
fn clock_command_parses_check_and_sync_and_rejects_the_rest() {
    assert!(
        clock_command::parse(&args("--config x --trace y"), false)
            .unwrap()
            .is_none()
    );
    assert!(
        clock_command::parse(&args("status --config x"), false)
            .unwrap()
            .is_none()
    );
    for good in [
        "clock sync",
        "clock check",
        "clock sync --threshold-ms 0",
        "clock sync --config bridge.toml --threshold-ms 5000",
    ] {
        assert!(clock_command::parse(&args(good), false).unwrap().is_some(), "{good}");
    }
    for bad in [
        "clock",
        "clock set",
        "clock sync --threshold-ms",
        "clock sync --threshold-ms -1",
        "clock sync --threshold-ms x",
        "clock sync --config",
        "clock sync --config a --config b",
        "clock sync extra",
    ] {
        assert_eq!(
            clock_command::parse(&args(bad), false).err(),
            Some(Error::Input),
            "{bad}"
        );
    }
    assert_eq!(
        clock_command::parse(&args("clock sync"), true).err(),
        Some(Error::Input)
    );
}

fn settle(outcome: Outcome, sync: bool, threshold_ms: i64, allowed: bool) -> (Result<(), Error>, String, Vec<i64>) {
    let mut set = vec![];
    let mut out = vec![];
    let result = clock_command::settle(outcome, (sync, threshold_ms), &mut out, |offset_ms| {
        set.push(offset_ms);
        allowed
    });
    (result, String::from_utf8(out).unwrap(), set)
}

#[test]
fn clock_sync_sets_the_clock_only_beyond_the_threshold_and_says_one_line() {
    let behind = Outcome::Synced {
        offset_ms: BEHIND_MS,
        hosts: 3,
    };
    assert_eq!(
        settle(behind, true, 1000, true),
        (
            Ok(()),
            "clock: offset_ms=63000000 hosts=3 action=set\n".into(),
            vec![BEHIND_MS]
        )
    );
    assert_eq!(
        settle(behind, false, 1000, true),
        (
            Ok(()),
            "clock: offset_ms=63000000 hosts=3 action=check\n".into(),
            vec![]
        )
    );
    assert_eq!(
        settle(behind, true, 1000, false),
        (
            Err(Error::Unavailable),
            "clock: offset_ms=63000000 hosts=3 action=denied\n".into(),
            vec![BEHIND_MS]
        )
    );
    for offset_ms in [-999, 0, 999] {
        let close = Outcome::Synced { offset_ms, hosts: 2 };
        assert_eq!(
            settle(close, true, 1000, true),
            (
                Ok(()),
                format!("clock: offset_ms={offset_ms} hosts=2 action=none\n"),
                vec![]
            )
        );
    }
    let ahead = Outcome::Synced {
        offset_ms: -1001,
        hosts: 2,
    };
    assert_eq!(settle(ahead, true, 1000, true).2, vec![-1001]);
}

#[test]
fn clock_sync_without_agreement_sets_nothing_and_fails() {
    let unsynced = Outcome::Unsynced {
        reason: "unreachable",
        answered: 0,
    };
    for sync in [true, false] {
        assert_eq!(
            settle(unsynced, sync, 1000, true),
            (
                Err(Error::Unavailable),
                "clock: unsynced reason=unreachable hosts=0\n".into(),
                vec![]
            )
        );
    }
}
