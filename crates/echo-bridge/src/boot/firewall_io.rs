use std::{
    fs, io,
    net::IpAddr,
    path::{Path, PathBuf},
};

use echo_boot::{Family, Result};
use tokio::runtime::Runtime;

use super::command_io;

const ADB_PORT: &str = "service.adb.tcp.port";

pub(super) struct FirewallIo {
    bin_dirs: Vec<PathBuf>,
    proc: PathBuf,
}

impl Default for FirewallIo {
    fn default() -> Self {
        Self {
            bin_dirs: ["/system/bin", "/system/xbin", "/vendor/bin"]
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            proc: PathBuf::from("/proc"),
        }
    }
}

impl FirewallIo {
    #[cfg(test)]
    pub(super) fn fixture(root: &Path) -> Self {
        Self {
            bin_dirs: vec![root.join("bin")],
            proc: root.join("proc"),
        }
    }

    pub(super) fn resolvers(&self, runtime: &Runtime) -> Result<Vec<IpAddr>> {
        Ok(self
            .required(runtime, "getprop", &[], None, "read Android resolver properties")?
            .lines()
            .filter_map(resolver)
            .collect())
    }

    pub(super) fn filter(&self, runtime: &Runtime, family: Family) -> Result<Option<String>> {
        let name = match family {
            Family::V4 => "iptables",
            Family::V6 => "ip6tables",
        };
        let Some(tool) = self.find(name)? else {
            return Ok(None);
        };
        let stage = match family {
            Family::V4 => "list IPv4 filter",
            Family::V6 => "list IPv6 filter",
        };
        command_io::run(runtime, &tool, &["-w", "-S"], None, stage).map(Some)
    }

    pub(super) fn restore(&self, runtime: &Runtime, family: Family, transaction: &str) -> Result<()> {
        let (name, stage) = match family {
            Family::V4 => ("iptables-restore", "restore IPv4 filter"),
            Family::V6 => ("ip6tables-restore", "restore IPv6 filter"),
        };
        self.required(runtime, name, &["-w", "--noflush"], Some(transaction), stage)?;
        Ok(())
    }

    pub(super) fn ipv6_disabled(&self, enforce: bool) -> Result<bool> {
        let base = self.proc.join("sys/net/ipv6/conf");
        let entries = fs::read_dir(&base).map_err(|_| "list IPv6 interfaces")?;
        let mut all = false;
        let mut default = false;
        let mut disabled = true;
        for entry in entries {
            let entry = entry.map_err(|_| "inspect IPv6 interface")?;
            let name = entry.file_name();
            all |= name == "all";
            default |= name == "default";
            disabled &= disable_ipv6(&entry.path().join("disable_ipv6"), enforce)?;
        }
        Ok(all && default && disabled)
    }

    pub(super) fn usb_adb_only(&self, runtime: &Runtime, enforce: bool) -> Result<bool> {
        let port = self.property(runtime, ADB_PORT)?;
        let listening = self.adb_listening()?;
        if port == "0" && !listening {
            return Ok(true);
        }
        if !enforce {
            return Ok(false);
        }
        if port != "0" {
            self.set_property(runtime, ADB_PORT, "0")?;
        }
        if self.property(runtime, "init.svc.adbd")? == "running" {
            self.set_property(runtime, "ctl.restart", "adbd")?;
        }
        Ok(self.property(runtime, ADB_PORT)? == "0" && !self.adb_listening()?)
    }

    fn adb_listening(&self) -> Result<bool> {
        for file in ["net/tcp", "net/tcp6"] {
            match fs::read_to_string(self.proc.join(file)) {
                Ok(table) if table.lines().any(adb_listener) => return Ok(true),
                Ok(_) => (),
                Err(error) if file == "net/tcp6" && error.kind() == io::ErrorKind::NotFound => (),
                Err(_) => return Err("inspect adb listeners"),
            }
        }
        Ok(false)
    }

    fn property(&self, runtime: &Runtime, key: &str) -> Result<String> {
        self.required(runtime, "getprop", &[key], None, "read adb property")
            .map(|value| value.trim().to_owned())
    }

    fn set_property(&self, runtime: &Runtime, key: &str, value: &str) -> Result<()> {
        self.required(runtime, "setprop", &[key, value], None, "close adb network listener")?;
        Ok(())
    }

    fn required(
        &self,
        runtime: &Runtime,
        name: &str,
        args: &[&str],
        input: Option<&str>,
        stage: &'static str,
    ) -> Result<String> {
        let tool = self.find(name)?.ok_or(stage)?;
        command_io::run(runtime, &tool, args, input, stage)
    }

    fn find(&self, name: &str) -> Result<Option<PathBuf>> {
        for dir in &self.bin_dirs {
            let tool = dir.join(name);
            match fs::symlink_metadata(&tool) {
                Ok(_) => return Ok(Some(tool)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(_) => return Err("inspect Android command"),
            }
        }
        Ok(None)
    }
}

fn resolver(line: &str) -> Option<IpAddr> {
    let (key, value) = line.strip_prefix('[')?.strip_suffix(']')?.split_once("]: [")?;
    let index = key
        .strip_prefix("dhcp.wlan0.dns")
        .or_else(|| key.strip_prefix("net.dns"))?;
    if index.is_empty() || !index.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let host = value.split_once('%').map_or(value, |(host, _)| host);
    host.parse().ok()
}

fn adb_listener(line: &str) -> bool {
    let mut fields = line.split_whitespace();
    let Some(local) = fields.nth(1) else { return false };
    local.ends_with(":15B3") && fields.nth(1) == Some("0A")
}

fn disable_ipv6(path: &Path, enforce: bool) -> Result<bool> {
    let read = || {
        fs::read_to_string(path)
            .map(|value| value.trim() == "1")
            .map_err(|_| "read IPv6 disable state")
    };
    let disabled = read()?;
    if disabled || !enforce {
        return Ok(disabled);
    }
    fs::write(path, b"1\n").map_err(|_| "disable IPv6 interface")?;
    read()
}
