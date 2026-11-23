//! T3 boot journey through both controller modes over one synthetic installation.

use std::{fs, sync::mpsc::sync_channel, thread, time::Duration};

use echo_boot::{Role, Slot};

use super::{
    boot_device_services,
    fake::Fake,
    filter_fixture::{FilterFixture, STOCK},
};
use crate::boot::{Mode, execute};

const END: u64 = 270_000;
const WAIT: Duration = Duration::from_secs(10);

#[test]
fn boot_assembled_repairs_flush_and_falls_back_then_parks_and_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let path = root.join("selection.json");
    let (mut filter, device) = boot_device_services::fixture(root);
    fs::create_dir(root.join("installed")).unwrap();
    filter.write("installed/base.apk", "fixture-apk:old");
    filter.write("bin/echo-screen.apk", "fixture-apk:new");
    filter.events.push((4999, |fixture| fixture.write("iptables", STOCK)));

    thread::scope(|scope| {
        let (advance, tick) = sync_channel::<u64>(0);
        let (waiting, next) = sync_channel::<u64>(0);
        let watcher_path = &path;
        let writer = scope.spawn(move || {
            let mut writer = Fake::new(watcher_path, END);
            writer.firewall = Some(FilterFixture::attach(root));
            writer.synchronize = Some(Box::new(move |time| {
                waiting.send(time).unwrap();
                assert_eq!(tick.recv_timeout(WAIT).unwrap(), time);
            }));
            execute(Mode::Firewall, &mut writer).unwrap();
            assert_eq!(writer.logs, ["firewall ready"]);
        });
        assert_eq!(next.recv_timeout(WAIT).unwrap(), 5000);
        filter.write("time", "0");
        let mut runner = Fake::new(&path, END);
        runner.ready_current = false;
        runner.parked = Some((255_000, 260_000));
        runner.samples = vec![4999, 5000];
        runner.services = Some(device);
        runner.firewall = Some(filter);
        let transcript = FilterFixture::attach(root);
        runner.synchronize = Some(Box::new(move |time| {
            if time.is_multiple_of(5000) {
                advance.send(time).unwrap();
                if time < END {
                    assert_eq!(next.recv_timeout(WAIT).unwrap(), time + 5000);
                    transcript.write("time", &time.to_string());
                }
            }
        }));
        execute(Mode::Run, &mut runner).unwrap();
        writer.join().unwrap();
        assert_journey(&mut runner);
    });
}

fn assert_journey(runner: &mut Fake<'_>) {
    assert_eq!(runner.times(Role::Core), [0, 63_000, 126_000, 189_000, 260_000]);
    assert_eq!(runner.times(Role::Device), [0, 189_000, 260_000]);
    assert_eq!(runner.acks, [(0, false), (255_000, true), (260_000, false)]);
    assert_eq!(
        runner.tries,
        [(0, 0), (60_000, 1), (123_000, 2), (186_000, 3), (249_000, 0)]
    );
    assert_eq!(runner.journal().slot(), Slot::Previous);
    assert!(
        runner
            .starts
            .iter()
            .filter(|start| start.0 >= 189_000)
            .all(|start| start.2 == Slot::Previous)
    );
    let filter = runner.firewall.as_ref().unwrap();
    assert_eq!(
        filter.restores(),
        [
            "0 iptables-restore -w --noflush",
            "0 ip6tables-restore -w --noflush",
            "5000 iptables-restore -w --noflush",
        ]
    );
    assert_eq!(filter.read("installed/base.apk"), "fixture-apk:new");
    assert_eq!(
        filter
            .read("device-commands")
            .lines()
            .filter(|line| line.contains("pm install -r"))
            .count(),
        1
    );
}
