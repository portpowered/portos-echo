//! T3 startup service effects through Android command subprocesses and the boot controller.

use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

use echo_boot::{Role, Slot};
use serde_json::{Value, json};
use tokio::runtime::Builder;

use super::{fake::Fake, filter_fixture::FilterFixture};
use crate::boot::{Mode, device_io::DeviceIo, execute, firewall_io::FirewallIo};

const CANDIDATE: &str = "fixture-apk:version2";
const INSTALLED: &str = "fixture-apk:version1";
const RESOLVER: &str = "[dhcp.wlan0.dns1]: [192.0.2.53]\n";
const USER_SETTING: &str = "synthetic owner setting";

pub(super) fn fixture(root: &Path) -> (FilterFixture, DeviceIo) {
    let filter = FilterFixture::new(root);
    let program = root.join("bin/device-command");
    fs::write(&program, include_str!("device-command.py")).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    for name in ["pm", "dumpsys", "am", "settings", "hwclock"] {
        symlink("device-command", root.join("bin").join(name)).unwrap();
    }
    filter.write("device-commands", "");
    filter.write("device.json", &original().to_string());
    let device = open(root);
    (filter, device)
}

fn open(root: &Path) -> DeviceIo {
    DeviceIo {
        commands: FirewallIo::fixture(root),
        runtime: Builder::new_current_thread().enable_all().build().unwrap(),
        journal: root.join("alexa.json"),
        apk: root.join("bin/echo-screen.apk"),
    }
}

fn original() -> Value {
    json!({
        "packages": {"com.amazon.bishop": 1, "com.amazon.paladin": 0, "com.amazon.whad": 2},
        "settings": {"global": {"device_provisioned": "0", "bluetooth_on": "1", "auto_time_zone": "1"},
            "secure": {"user_setup_complete": "null"},
            "system": {"screen_off_timeout": "60000", "owner_value": USER_SETTING}}
    })
}

fn state(filter: &FilterFixture) -> Value {
    serde_json::from_str(&filter.read("device.json")).unwrap()
}

fn actions(filter: &FilterFixture, action: &str) -> usize {
    filter
        .read("device-commands")
        .lines()
        .filter(|line| line.contains(action))
        .count()
}

#[test]
fn boot_device_services_restores_default_enabled_disabled_and_unset_values() {
    let dir = tempfile::tempdir().unwrap();
    let (filter, device) = fixture(dir.path());
    for _ in 0..2 {
        device.prepare(|stage| assert_eq!(stage, "screen APK unavailable"));
    }
    let disabled = state(&filter);
    assert_eq!(
        disabled["packages"],
        json!({"com.amazon.bishop":3,"com.amazon.paladin":3,"com.amazon.whad":2})
    );
    assert_eq!(disabled["settings"]["system"]["smartconn_status"], "enabled");
    assert_eq!(disabled["settings"]["system"]["screen_off_timeout"], "300000");
    assert_eq!(disabled["settings"]["global"]["auto_time_zone"], "1");
    assert_eq!(actions(&filter, "pm disable-user"), 2);
    assert_eq!(actions(&filter, "settings --user 0 put"), 5);
    assert_eq!(actions(&filter, "--ez val false"), 2);

    let restarted = open(dir.path());
    for _ in 0..2 {
        restarted.restore(|stage| panic!("{stage}")).unwrap();
    }
    assert_eq!(state(&filter), original());
    assert_eq!(actions(&filter, "pm enable --user 0 com.amazon.bishop"), 1);
    assert_eq!(actions(&filter, "pm default-state --user 0 com.amazon.paladin"), 1);
    assert_eq!(actions(&filter, "settings --user 0 delete system smartconn_status"), 1);
    assert_eq!(actions(&filter, "--ez val true"), 2);
    let journal: Value = serde_json::from_str(&filter.read("alexa.json")).unwrap();
    assert_eq!(journal, json!({"version":1,"packages":{},"settings":[]}));
}

#[test]
fn boot_device_services_retries_failed_apply_and_restore_with_durable_original() {
    let dir = tempfile::tempdir().unwrap();
    let (filter, device) = fixture(dir.path());
    filter.write("fail", "pm disable-user --user 0 com.amazon.bishop");
    let mut log = Vec::new();
    device.prepare(|stage| log.push(stage));
    assert_eq!(log, ["change Alexa package state", "screen APK unavailable"]);
    assert_eq!(state(&filter)["packages"]["com.amazon.bishop"], 1);
    let pending: Value = serde_json::from_str(&filter.read("alexa.json")).unwrap();
    assert_eq!(pending["packages"]["com.amazon.bishop"], "enabled");

    filter.remove("fail");
    let rebooted = open(dir.path());
    rebooted.prepare(|stage| assert_eq!(stage, "screen APK unavailable"));
    assert_eq!(state(&filter)["packages"]["com.amazon.bishop"], 3);
    filter.write("fail", "settings --user 0 put system screen_off_timeout 60000");
    assert_eq!(rebooted.restore(|_| ()), Err("Alexa restore incomplete"));
    let pending: Value = serde_json::from_str(&filter.read("alexa.json")).unwrap();
    assert_eq!(
        pending["settings"],
        json!([{"namespace":"system","key":"screen_off_timeout","previous":"60000"}])
    );
    assert_eq!(pending["packages"], json!({}));
    assert_eq!(state(&filter)["settings"]["system"]["owner_value"], USER_SETTING);
    filter.remove("fail");
    open(dir.path()).restore(|stage| panic!("{stage}")).unwrap();
    assert_eq!(state(&filter), original());
}

#[test]
fn boot_device_services_installs_only_on_difference_and_launches_available_screen() {
    let dir = tempfile::tempdir().unwrap();
    let (filter, device) = fixture(dir.path());
    filter.write("bin/echo-screen.apk", INSTALLED);
    device.prepare(|stage| panic!("{stage}"));
    filter.write("bin/echo-screen.apk", CANDIDATE);
    device.prepare(|stage| panic!("{stage}"));
    device.prepare(|stage| panic!("{stage}"));
    filter.remove("bin/echo-screen.apk");
    device.prepare(|stage| panic!("{stage}"));
    assert_eq!(filter.read("installed/base.apk"), CANDIDATE);
    assert_eq!(actions(&filter, "pm install -r"), 2);
    assert_eq!(actions(&filter, "am start -W"), 4);
}

#[test]
fn boot_device_services_preserves_prior_screen_when_candidate_or_activity_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (filter, device) = fixture(dir.path());
    device.prepare(|stage| assert_eq!(stage, "screen APK unavailable"));
    assert_eq!(actions(&filter, "am start -W"), 0);
    fs::create_dir(dir.path().join("installed")).unwrap();
    filter.write("installed/base.apk", INSTALLED);
    filter.write("bin/echo-screen.apk", "corrupt");
    let mut log = Vec::new();
    device.prepare(|stage| log.push(stage));
    filter.write("fail", "am start -W --user 0 -n org.portos.echoscreen/.MainActivity");
    device.prepare(|stage| log.push(stage));
    assert_eq!(
        log,
        [
            "install staged screen APK",
            "install staged screen APK",
            "launch installed screen activity"
        ]
    );
    assert_eq!(filter.read("installed/base.apk"), INSTALLED);
    assert_eq!(actions(&filter, "am start -W"), 2);
}

#[test]
fn boot_device_services_settles_new_resolver_and_writes_rtc_on_clock_adjustment() {
    let dir = tempfile::tempdir().unwrap();
    let (mut timeline, device) = fixture(dir.path());
    timeline.write("resolvers", "");
    timeline
        .events
        .push((2000, |fixture| fixture.write("resolvers", RESOLVER)));
    let path = dir.path().join("selection.json");
    let mut host = Fake::new(&path, 10_100);
    host.services = Some(device);
    host.service_timeline = Some(timeline);
    execute(Mode::Run, &mut host).unwrap();
    assert_eq!(host.times(Role::Device), [0]);
    assert_eq!(host.times(Role::Core), [7000]);
    assert_eq!(host.clock_starts, [7000]);
    assert!(host.clock_stops.is_empty());
    let timeline = host.service_timeline.unwrap();
    assert_eq!(actions(&timeline, "7100 hwclock -w -u"), 1);
    assert_eq!(actions(&timeline, "hwclock"), 1);
    assert_eq!(state(&timeline)["settings"]["global"]["auto_time_zone"], "1");
}

#[test]
fn boot_device_services_terminates_stalled_clock_at_deadline_or_link_loss() {
    for (loss, deadline, stage) in [
        (false, 6000, "clock sync timed out"),
        (true, 2000, "clock sync lost network"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (mut timeline, device) = fixture(dir.path());
        if loss {
            timeline
                .events
                .push((2000, |fixture| fixture.write("sys/class/net/wlan0/carrier", "0\n")));
        }
        let path = dir.path().join("selection.json");
        let mut host = Fake::new(&path, 7500);
        host.services = Some(device);
        host.service_timeline = Some(timeline);
        host.clock_delay = None;
        host.exits.push((100, Role::Device));
        execute(Mode::Run, &mut host).unwrap();
        assert_eq!(host.times(Role::Device), [0, 3100]);
        assert_eq!(host.times(Role::Core), [0]);
        assert_eq!(host.clock_starts, [0]);
        assert_eq!(host.clock_stops, [deadline]);
        assert!(host.logs.iter().any(|line| line == stage));
        assert!(
            host.logs
                .iter()
                .all(|line| !line.contains(USER_SETTING) && !line.contains("192.0.2.53"))
        );
        let timeline = host.service_timeline.unwrap();
        assert_eq!(actions(&timeline, "hwclock"), 0);
        assert_eq!(state(&timeline)["settings"]["global"]["auto_time_zone"], "1");
        assert_eq!(state(&timeline)["settings"]["system"]["owner_value"], USER_SETTING);
    }
}

#[test]
fn boot_device_services_starts_offline_core_at_ten_seconds_and_accepts_ready() {
    let dir = tempfile::tempdir().unwrap();
    let (timeline, device) = fixture(dir.path());
    timeline.write("resolvers", "");
    let path = dir.path().join("selection.json");
    let mut host = Fake::new(&path, 70_100);
    host.services = Some(device);
    host.service_timeline = Some(timeline);
    execute(Mode::Run, &mut host).unwrap();
    assert_eq!(host.times(Role::Device), [0]);
    assert_eq!(host.times(Role::Core), [10_000]);
    assert!(host.clock_starts.is_empty());
    assert_eq!(host.tries, [(0, 0)]);
    assert_eq!(host.journal().slot(), Slot::Current);
}
