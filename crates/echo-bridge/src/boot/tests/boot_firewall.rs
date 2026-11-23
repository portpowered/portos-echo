//! T3 controlled firewall commands through the Android I/O adapter.

use echo_boot::Role;

use super::{
    fake::Fake,
    filter_fixture::{FilterFixture, GOLDEN4, GOLDEN6, STOCK},
};
use crate::boot::{Mode, execute};

const RESTORE4: &str = "iptables-restore -w --noflush";
const RESTORE6: &str = "ip6tables-restore -w --noflush";
const STOCK_RULE: &str = "-A stock_net -o wlan0 -j RETURN";

#[test]
fn boot_firewall_matches_athena_golden_and_closes_network_adb() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("selection.json");
    let mut host = Fake::new(&journal, 5100);
    let fixture = FilterFixture::new(dir.path());
    fixture.write("adb-port", "5555");
    fixture.write(
        "proc/net/tcp6",
        " 0: 00000000000000000000000000000000:15B3 00000000:0000 0A\n",
    );
    host.firewall = Some(fixture);

    execute(Mode::Firewall, &mut host).unwrap();

    let fixture = host.firewall.unwrap();
    assert_eq!(canonical(&fixture.read("iptables")), canonical(GOLDEN4));
    assert_eq!(canonical(&fixture.read("ip6tables")), canonical(GOLDEN6));
    assert_eq!(fixture.restores(), [format!("0 {RESTORE4}"), format!("0 {RESTORE6}")]);
    assert_eq!(
        fixture
            .read("commands")
            .lines()
            .filter(|l| l.contains("setprop"))
            .collect::<Vec<_>>(),
        ["0 setprop service.adb.tcp.port 0", "0 setprop ctl.restart adbd"]
    );
    assert_eq!(fixture.read("adb-port"), "0");
    assert_eq!(fixture.read("proc/net/tcp6"), "");
    assert_eq!(host.logs, ["firewall ready"]);
}

#[test]
fn boot_firewall_repairs_stock_flush_at_five_seconds_and_replaces_dns_exceptions() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("selection.json");
    let mut host = Fake::new(&journal, 10100);
    host.samples = vec![4999, 9999];
    let mut fixture = FilterFixture::new(dir.path());
    fixture.events = vec![(4999, flush), (9999, renew)];
    host.firewall = Some(fixture);

    execute(Mode::Firewall, &mut host).unwrap();

    let fixture = host.firewall.unwrap();
    assert_eq!(
        fixture.restores(),
        [
            format!("0 {RESTORE4}"),
            format!("0 {RESTORE6}"),
            format!("5000 {RESTORE4}"),
            format!("10000 {RESTORE4}")
        ]
    );
    let expected = GOLDEN4.replace("192.0.2.53", "198.51.100.53").replace(
        "-A OUTPUT -j echo_out\n",
        "-A OUTPUT -j echo_out\n-A OUTPUT -j stock_net\n",
    );
    let expected = format!("{expected}-N stock_net\n{STOCK_RULE}\n");
    assert_eq!(canonical(&fixture.read("iptables")), canonical(&expected));
    assert_eq!(canonical(&fixture.read("ip6tables")), canonical(GOLDEN6));
    assert_eq!(host.logs, ["firewall ready"]);
}

fn flush(fixture: &FilterFixture) {
    let listing = format!("{STOCK}-N stock_net\n{STOCK_RULE}\n");
    fixture.write("iptables", &listing);
    assert_eq!(fixture.restores(), [format!("0 {RESTORE4}"), format!("0 {RESTORE6}")]);
}

fn renew(fixture: &FilterFixture) {
    assert_eq!(
        fixture.restores(),
        [
            format!("0 {RESTORE4}"),
            format!("0 {RESTORE6}"),
            format!("5000 {RESTORE4}")
        ]
    );
    let listing = fixture.read("iptables").replace(
        "-A OUTPUT -j echo_out\n",
        "-A OUTPUT -j stock_net\n-A OUTPUT -j echo_out\n-A OUTPUT -j echo_out\n",
    );
    fixture.write("iptables", &listing);
    fixture.write(
        "resolvers",
        "[dhcp.wlan0.dns1]: [198.51.100.53]\n[net.dns2]: [2001:db8::53]\n",
    );
}

#[test]
fn boot_firewall_missing_ipv6_filter_disables_all_current_and_new_interfaces() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("selection.json");
    let mut host = Fake::new(&journal, 5100);
    let mut fixture = FilterFixture::new(dir.path());
    fixture.remove("bin/ip6tables");
    for iface in ["all", "default", "lo", "wlan0"] {
        fixture.interface(iface, "0\n");
    }
    fixture.events.push((5000, |fixture| {
        assert_eq!(fixture.disabled("default"), "1\n");
        fixture.interface("eth0", "0\n");
    }));
    host.firewall = Some(fixture);

    execute(Mode::Firewall, &mut host).unwrap();

    let fixture = host.firewall.unwrap();
    assert_eq!(fixture.restores(), [format!("0 {RESTORE4}")]);
    for iface in ["all", "default", "lo", "wlan0", "eth0"] {
        assert_eq!(fixture.disabled(iface), "1\n");
    }
    assert_eq!(host.logs, ["firewall ready"]);
}

#[test]
fn boot_firewall_retries_installed_tool_failures_without_disabling_ipv6() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("selection.json");
    let mut host = Fake::new(&journal, 10100);
    let mut fixture = FilterFixture::new(dir.path());
    fixture.interface("default", "0\n");
    fixture.write("ip6tables.fail", "");
    fixture.write("ip6tables-restore.fail", "");
    fixture.events = vec![
        (5000, |fixture| fixture.remove("ip6tables.fail")),
        (10000, |fixture| fixture.remove("ip6tables-restore.fail")),
    ];
    host.firewall = Some(fixture);

    execute(Mode::Firewall, &mut host).unwrap();

    let fixture = host.firewall.unwrap();
    assert_eq!(fixture.disabled("default"), "0\n");
    assert_eq!(
        fixture.restores(),
        [
            format!("0 {RESTORE4}"),
            format!("5000 {RESTORE6}"),
            format!("10000 {RESTORE6}")
        ]
    );
    assert_eq!(canonical(&fixture.read("ip6tables")), canonical(GOLDEN6));
    assert_eq!(
        host.logs,
        [
            "firewall unhealthy: list IPv6 filter",
            "firewall unhealthy: restore IPv6 filter",
            "firewall ready"
        ]
    );
}

#[test]
fn boot_firewall_run_withholds_both_roles_until_live_protection_is_confirmed() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("selection.json");
    let mut host = Fake::new(&journal, 5100);
    let mut fixture = FilterFixture::new(dir.path());
    fixture.write("iptables", GOLDEN4);
    fixture
        .events
        .push((5000, |fixture| fixture.write("ip6tables", GOLDEN6)));
    host.firewall = Some(fixture);

    execute(Mode::Run, &mut host).unwrap();

    assert_eq!(host.times(Role::Device), [5000]);
    assert_eq!(host.times(Role::Core), [5000]);
    assert_eq!(host.acks, [(5000, false)]);
    assert_eq!(host.logs[0], "firewall unhealthy: IPv6 firewall unconfirmed");
    assert_eq!(host.firewall.unwrap().restores(), Vec::<String>::new());
}

fn canonical(listing: &str) -> Vec<&str> {
    let mut lines: Vec<_> = listing.lines().collect();
    lines.sort_by_key(|line| line.split_whitespace().take(2).collect::<Vec<_>>());
    lines
}
