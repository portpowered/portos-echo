use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};

use echo_boot::Family;
use tokio::runtime::{Builder, Runtime};

use super::{super::firewall_io::FirewallIo, fixture_result};

pub(super) const GOLDEN4: &str = include_str!("firewall-v4.rules");
pub(super) const GOLDEN6: &str = include_str!("firewall-v6.rules");
pub(super) const STOCK: &str = "-P INPUT ACCEPT\n-P OUTPUT DROP\n";

pub(super) type TimedFilterEvent = (u64, fn(&FilterFixture));

pub(super) struct FilterFixture {
    root: PathBuf,
    pub io: FirewallIo,
    pub runtime: Runtime,
    pub events: Vec<TimedFilterEvent>,
}

impl FilterFixture {
    pub(super) fn new(root: &Path) -> Self {
        let fixture = Self::attach(root);
        fixture_result(fs::create_dir_all(root.join("bin")));
        fixture_result(fs::create_dir_all(root.join("proc/net")));
        let program = root.join("bin/command");
        fixture_result(fs::write(&program, include_str!("filter-command.py")));
        fixture_result(fs::set_permissions(&program, fs::Permissions::from_mode(0o700)));
        for name in [
            "iptables",
            "ip6tables",
            "iptables-restore",
            "ip6tables-restore",
            "getprop",
            "setprop",
        ] {
            fixture_result(symlink("command", root.join("bin").join(name)));
        }
        fixture.write("commands", "");
        fixture.write("time", "0");
        fixture.write("iptables", STOCK);
        fixture.write("ip6tables", STOCK);
        fixture.write("adb-port", "0");
        fixture.write("proc/net/tcp", "");
        fixture.write("proc/net/tcp6", "");
        fixture_result(fs::create_dir_all(root.join("sys/class/net/wlan0")));
        fixture.write("sys/class/net/wlan0/carrier", "1\n");
        fixture.write(
            "resolvers",
            "[dhcp.wlan0.dns1]: [192.0.2.53]\n[net.dns1]: [192.0.2.53]\n[net.dns2]: [2001:db8::53%wlan0]\n",
        );
        fixture
    }

    pub(super) fn attach(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            io: FirewallIo::fixture(root),
            runtime: fixture_result(Builder::new_current_thread().enable_all().build()),
            events: vec![],
        }
    }

    pub(super) fn at(&self, previous: u64, now: u64) {
        self.write("time", &now.to_string());
        for &(time, event) in &self.events {
            if previous < time && time <= now {
                event(self);
            }
        }
    }

    pub(super) fn write(&self, name: &str, value: &str) {
        fixture_result(fs::write(self.root.join(name), value));
    }

    pub(super) fn read(&self, name: &str) -> String {
        fixture_result(fs::read_to_string(self.root.join(name)))
    }

    pub(super) fn remove(&self, name: &str) {
        fixture_result(fs::remove_file(self.root.join(name)));
    }

    pub(super) fn interface(&self, name: &str, initial: &str) {
        let dir = self.root.join("proc/sys/net/ipv6/conf").join(name);
        fixture_result(fs::create_dir_all(&dir));
        fixture_result(fs::write(dir.join("disable_ipv6"), initial));
    }

    pub(super) fn disabled(&self, name: &str) -> String {
        self.read(&format!("proc/sys/net/ipv6/conf/{name}/disable_ipv6"))
    }

    pub(super) fn restores(&self) -> Vec<String> {
        self.read("commands")
            .lines()
            .filter(|line| line.contains("-restore"))
            .map(str::to_owned)
            .collect()
    }
}

pub(super) const fn golden(family: Family) -> &'static str {
    match family {
        Family::V4 => GOLDEN4,
        Family::V6 => GOLDEN6,
    }
}
