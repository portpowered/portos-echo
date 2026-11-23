//! T3 controlled supervision using isolated child processes and a paused host clock.

mod boot_assembled;
mod boot_device_services;
mod boot_firewall;
mod fake;
mod filter_fixture;

use echo_boot::{Identity, Role, Slot, Stop, supervise};
use fake::Fake;

const DEVICE: Identity = Identity {
    uid: 1000,
    real_gid: 3990,
    effective_gid: 1000,
    groups: &[1000, 1005, 3003, 1004, 1002],
    domain: Some("u:r:hassmic:s0"),
};
const CORE: Identity = Identity {
    uid: 9999,
    real_gid: 3990,
    effective_gid: 3990,
    groups: &[3990, 3003, 1004],
    domain: None,
};

fn fixture_result<T, E: std::fmt::Debug>(result: std::result::Result<T, E>) -> T {
    result.unwrap_or_else(|error| unreachable!("boot fixture error: {error:?}"))
}

#[test]
fn boot_supervision_restarts_each_role_ten_times() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let mut host = Fake::new(&path, 652_000);
    for n in 0..10 {
        host.exits.push((61_000 + n * 65_000, Role::Device));
        host.exits.push((62_000 + n * 65_000, Role::Core));
    }
    supervise(&mut host).unwrap();
    for (role, first, identity) in [(Role::Device, 61_000, DEVICE), (Role::Core, 62_000, CORE)] {
        let restarted: Vec<_> = (0..10).map(|n| first + n * 65_000 + 3_000).collect();
        assert_eq!(host.times(role), [vec![0], restarted].concat());
        let exits: Vec<_> = host
            .stops
            .iter()
            .filter(|stop| stop.1 == role && stop.2 == Stop::Exited)
            .collect();
        assert_eq!(exits.len(), 10);
        assert!(exits.iter().all(|stop| stop.3 == "exit status: 7"));
        assert!(
            host.starts
                .iter()
                .filter(|start| start.1 == role)
                .all(|start| start.3 == identity)
        );
    }
    assert_eq!(host.logs.iter().filter(|log| log.starts_with("start ")).count(), 22);
    assert_eq!(host.logs.iter().filter(|log| log.starts_with("exit ")).count(), 22);
}

#[test]
fn boot_supervision_falls_back_after_three_deadlines_and_resets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let mut host = Fake::new(&path, 250_000);
    host.ready_current = false;
    supervise(&mut host).unwrap();
    assert_eq!(host.times(Role::Core), [0, 63_000, 126_000, 189_000]);
    assert_eq!(host.times(Role::Device), [0, 189_000]);
    assert_eq!(
        host.tries,
        [(0, 0), (60_000, 1), (123_000, 2), (186_000, 3), (249_000, 0)]
    );
    assert_eq!(
        host.starts
            .iter()
            .filter(|s| s.2 == Slot::Previous)
            .map(|s| s.1)
            .collect::<Vec<_>>(),
        [Role::Device, Role::Core]
    );
    assert_eq!(host.stops.iter().filter(|stop| stop.2 == Stop::SwitchSlot).count(), 1);
    assert_eq!(
        host.logs
            .iter()
            .filter(|line| line.starts_with("fallback "))
            .collect::<Vec<_>>(),
        [&format!("fallback {} {}", "a".repeat(64), "b".repeat(64))]
    );
    assert_eq!(host.journal().slot(), Slot::Previous);

    let mut promoted = Fake::new(&path, 1000);
    promoted.current = "c".repeat(64);
    supervise(&mut promoted).unwrap();
    assert_eq!(
        promoted.starts.iter().map(|s| s.2).collect::<Vec<_>>(),
        [Slot::Current, Slot::Current]
    );
    assert_eq!(promoted.journal().failed_tries(), 0);
}

#[test]
fn boot_supervision_counts_interrupted_attempt_once_and_honors_exact_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let mut interrupted = Fake::new(&path, 200);
    supervise(&mut interrupted).unwrap();
    assert_eq!(interrupted.journal().failed_tries(), 0);

    let mut restarted = Fake::new(&path, 60_100);
    restarted.samples = vec![59_999, 60_000];
    supervise(&mut restarted).unwrap();
    assert_eq!(restarted.tries, [(0, 1), (60_000, 0)]);
    assert_eq!(restarted.journal().failed_tries(), 0);
    let mut verified = Fake::new(&path, 100);
    verified.parked = Some((0, 1000));
    supervise(&mut verified).unwrap();
    assert_eq!(verified.journal().failed_tries(), 0);
    assert_eq!(verified.acks, [(0, true)]);
}

#[test]
fn boot_supervision_parks_until_marker_is_removed_without_charging() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let mut host = Fake::new(&path, 220_000);
    host.parked = Some((20_000, 155_000));
    supervise(&mut host).unwrap();
    assert_eq!(host.acks, [(0, false), (20_000, true), (155_000, false)]);
    assert_eq!(host.times(Role::Core), [0, 155_000]);
    assert_eq!(host.times(Role::Device), [0, 155_000]);
    assert_eq!(
        host.stops
            .iter()
            .filter(|stop| stop.2 == Stop::Requested)
            .map(|stop| stop.0)
            .collect::<Vec<_>>(),
        [20_000, 20_000]
    );
    assert_eq!(host.tries, [(0, 0)]);
}

#[test]
fn boot_supervision_parks_during_backoff_and_reopens_a_stopped_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let mut host = Fake::new(&path, 10_100);
    host.exits = vec![(100, Role::Core), (3200, Role::Core)];
    host.parked = Some((4000, 10_000));
    supervise(&mut host).unwrap();
    assert_eq!(host.times(Role::Core), [0, 3100, 10_000]);
    assert_eq!(host.tries, [(0, 0), (100, 1), (3200, 2)]);
    assert_eq!(host.acks, [(0, false), (4000, true), (10_000, false)]);

    let mut stopped = Fake::new(&path, 1000);
    stopped.parked = Some((0, 2000));
    supervise(&mut stopped).unwrap();
    assert_eq!(stopped.starts.as_slice(), &[]);
    assert_eq!(stopped.journal().failed_tries(), 2);

    let mut reopened = Fake::new(&path, 200);
    supervise(&mut reopened).unwrap();
    assert_eq!(reopened.times(Role::Core), [0]);
    assert_eq!(reopened.journal().failed_tries(), 2);
}

#[test]
fn boot_supervision_quick_exit_backoff_caps_and_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let delays = [
        3_000, 6_000, 12_000, 24_000, 48_000, 96_000, 192_000, 300_000, 300_000, 3_000,
    ];
    let mut start = 0;
    let mut expected = vec![start];
    let mut host = Fake::new(&path, 1);
    for (index, delay) in delays.into_iter().enumerate() {
        let elapsed = if index == 9 { 60_000 } else { 100 };
        host.exits.push((start + elapsed, Role::Device));
        start += elapsed + delay;
        expected.push(start);
    }
    host.end = start + 100;
    supervise(&mut host).unwrap();
    assert_eq!(host.times(Role::Device), expected);
    assert_eq!(host.times(Role::Core), [0]);
}

#[test]
fn boot_supervision_bad_layout_and_privileges_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let mut host = Fake::new(&path, 150);
    host.deny = Some(Role::Core);
    assert_eq!(supervise(&mut host), Err("denied role setup"));
    assert_eq!(host.times(Role::Core), Vec::<u64>::new());
    assert_eq!(host.stops.iter().map(|stop| stop.1).collect::<Vec<_>>(), [Role::Device]);

    let mut host = Fake::new(&path, 200_000);
    host.ready_current = false;
    host.previous = None;
    assert_eq!(supervise(&mut host), Err("open selected binary"));
    assert_eq!(host.times(Role::Core), [0, 63_000]);
    assert_eq!(host.journal().failed_tries(), 3);
    assert_eq!(host.journal().slot(), Slot::Current);
    let mut again = Fake::new(&path, 200_000);
    again.previous = None;
    assert_eq!(supervise(&mut again), Err("open selected binary"));
    assert_eq!(again.starts.as_slice(), &[]);

    std::fs::write(&path, b"{invalid").unwrap();
    let mut corrupt = Fake::new(&path, 200);
    assert_eq!(supervise(&mut corrupt), Err("invalid selection journal"));
    assert_eq!(corrupt.starts.as_slice(), &[]);
}

#[test]
fn boot_supervision_receipt_requires_a_complete_bounded_line_from_this_child() {
    use std::sync::OnceLock;
    let mut output = "x".repeat(4096);
    output.push_str("{\"event\":\"bridge.ready\"}\n{\"event\":\"bridge.ready\"}");
    let receipt = OnceLock::new();
    super::receipt::drain(output.as_bytes(), &receipt).unwrap();
    assert_eq!(receipt.get(), None);
    let new_pipe = OnceLock::new();
    super::receipt::drain(b"{\"event\":\"bridge.ready\"}\n".as_slice(), &new_pipe).unwrap();
    assert!(new_pipe.get().is_some());
    assert_eq!(receipt.get(), None);
}
