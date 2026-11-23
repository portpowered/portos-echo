use std::{fmt::Write as _, net::IpAddr, time::Duration};

use crate::{HostOps, Log, Result};

pub(crate) const WATCH: Duration = Duration::from_secs(5);
const HOOK: &str = "-A OUTPUT -j echo_out";
const ADB_GUARD: &str = "-A INPUT ! -i lo -p tcp -m tcp --dport 5555 -j DROP";
const INPUT_POLICY: &str = "-P INPUT DROP";
const LOCAL4: &[&str] = &[
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "224.0.0.0/4",
    "255.255.255.255/32",
];
const LOCAL6: &[&str] = &["fe80::/10", "fc00::/7", "ff02::/16"];
const TRANSPORT: &[&str] = &[
    "-A OUTPUT -o wlan0 -j ACCEPT",
    "-A OUTPUT -o lo -j ACCEPT",
    "-A INPUT -i lo -j ACCEPT",
    "-A INPUT -i wlan0 -p tcp -m state --state RELATED,ESTABLISHED -j ACCEPT",
    "-A INPUT -i wlan0 -p udp -m state --state RELATED,ESTABLISHED -j ACCEPT",
];
const V4_TRANSPORT: &[&str] = &[
    "-A INPUT -i wlan0 -p udp -m udp --sport 67 --dport 68 -j ACCEPT",
    "-A INPUT -p icmp -m state --state RELATED,ESTABLISHED -j ACCEPT",
];
const V6_TRANSPORT: &[&str] = &[
    "-A INPUT -p ipv6-icmp -j ACCEPT",
    "-A INPUT -i wlan0 -p udp -m udp --sport 547 --dport 546 -j ACCEPT",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Family {
    V4,
    V6,
}

/// Runs the sole firewall writer until shutdown. Transient drift and command
/// failures are reported and retried on the next watch tick.
pub fn watch_firewall(host: &mut impl HostOps) {
    let mut last = None;
    while !host.shutdown() {
        let next = host.now() + WATCH;
        let health = inspect(host, true);
        if last != Some(health) {
            host.log(match health {
                Ok(()) => Log::FirewallReady,
                Err(stage) => Log::FirewallUnhealthy(stage),
            });
            last = Some(health);
        }
        while !host.shutdown() && host.now() < next {
            host.sleep(next.saturating_sub(host.now()));
        }
    }
}

/// Checks the live firewall and adb state without making any changes.
///
/// # Errors
/// Returns a stage error if any protection is absent or could not be verified.
pub(crate) fn check_firewall(host: &mut impl HostOps) -> Result<()> {
    inspect(host, false)
}

fn inspect(host: &mut impl HostOps, enforce: bool) -> Result<()> {
    let (mut resolvers, resolver_status) = match host.resolvers() {
        Ok(resolvers) => (resolvers, Ok(())),
        Err(stage) => (Vec::new(), Err(stage)),
    };
    resolvers.sort_unstable();
    resolvers.dedup();
    let v4 = inspect_family(host, Family::V4, &resolvers, enforce);
    let v6 = inspect_family(host, Family::V6, &resolvers, enforce);
    let adb = host.usb_adb_only(enforce).and_then(|closed| {
        if closed {
            Ok(())
        } else {
            Err("USB-only adb unconfirmed")
        }
    });
    v4.and(v6).and(adb).and(resolver_status)
}

fn inspect_family(host: &mut impl HostOps, family: Family, resolvers: &[IpAddr], enforce: bool) -> Result<()> {
    let Some(listing) = host.filter(family)? else {
        return match family {
            Family::V4 => Err("IPv4 filter tool missing"),
            Family::V6 if host.ipv6_disabled(enforce)? => Ok(()),
            Family::V6 => Err("IPv6 disable unconfirmed"),
        };
    };
    let wanted = Rules::new(family, resolvers);
    let installed = Table::new(&listing);
    if wanted.matches(&installed) {
        return Ok(());
    }
    if enforce {
        host.restore_filter(family, &wanted.transaction(&installed))?;
        if host
            .filter(family)?
            .is_some_and(|text| wanted.matches(&Table::new(&text)))
        {
            return Ok(());
        }
    }
    Err(match family {
        Family::V4 => "IPv4 firewall unconfirmed",
        Family::V6 => "IPv6 firewall unconfirmed",
    })
}

struct Rules {
    chain: Vec<String>,
    transport: Vec<&'static str>,
}

impl Rules {
    fn new(family: Family, resolvers: &[IpAddr]) -> Self {
        let (local, transport) = match family {
            Family::V4 => (LOCAL4, V4_TRANSPORT),
            Family::V6 => (LOCAL6, V6_TRANSPORT),
        };
        let mut chain = vec![
            "-A echo_out -p tcp -m tcp --dport 5555 -m owner ! --uid-owner 0 -j DROP".into(),
            "-A echo_out -o lo -j RETURN".into(),
        ];
        chain.extend(local.iter().map(|range| format!("-A echo_out -d {range} -j RETURN")));
        for addr in resolvers.iter().filter(|addr| addr.is_ipv4() == (family == Family::V4)) {
            let prefix = if addr.is_ipv4() { 32 } else { 128 };
            for protocol in ["udp", "tcp"] {
                chain.push(format!(
                    "-A echo_out -d {addr}/{prefix} -p {protocol} -m {protocol} --dport 53 -j RETURN"
                ));
            }
        }
        chain.push("-A echo_out -m owner --gid-owner 3990 -j RETURN".into());
        chain.push("-A echo_out -j DROP".into());
        Self {
            chain,
            transport: [TRANSPORT, transport].concat(),
        }
    }

    fn matches(&self, table: &Table) -> bool {
        table.contains(INPUT_POLICY)
            && table.contains("-N echo_out")
            && table.rules("echo_out").eq(self.chain.iter().map(String::as_str))
            && table.hook_at_head("OUTPUT", HOOK)
            && table.hook_at_head("INPUT", ADB_GUARD)
            && self.transport.iter().all(|rule| table.contains(rule))
    }

    fn transaction(&self, table: &Table) -> String {
        // The fork's MIT-licensed lockdown.sh uses one --noflush filter
        // transaction. Declaring a user chain flushes only that chain.
        let mut out = String::from("*filter\n:echo_out - [0:0]\n-P INPUT DROP\n");
        for rule in &self.chain {
            let _ = writeln!(out, "{rule}");
        }
        for (chain, rule) in [("OUTPUT", HOOK), ("INPUT", ADB_GUARD)] {
            let spec = &rule[3 + chain.len() + 1..];
            for _ in 0..table.count(rule) {
                let _ = writeln!(out, "-D {chain} {spec}");
            }
            let _ = writeln!(out, "-I {chain} 1 {spec}");
        }
        for &rule in &self.transport {
            if !table.contains(rule) {
                let _ = writeln!(out, "{rule}");
            }
        }
        out.push_str("COMMIT\n");
        out
    }
}

struct Table(Vec<String>);

impl Table {
    fn new(listing: &str) -> Self {
        Self(listing.lines().map(normalize).collect())
    }

    fn contains(&self, rule: &str) -> bool {
        self.0.iter().any(|line| line == rule)
    }

    fn count(&self, rule: &str) -> usize {
        self.0.iter().filter(|line| *line == rule).count()
    }

    fn rules<'a>(&'a self, chain: &'a str) -> impl Iterator<Item = &'a str> {
        self.0.iter().map(String::as_str).filter(move |line| {
            line.strip_prefix("-A ")
                .and_then(|s| s.split_once(' '))
                .is_some_and(|(name, _)| name == chain)
        })
    }

    fn hook_at_head(&self, chain: &str, rule: &str) -> bool {
        self.rules(chain).next() == Some(rule) && self.count(rule) == 1
    }
}

fn normalize(line: &str) -> String {
    let mut tokens = line.split_whitespace();
    let mut rule = Vec::new();
    while let Some(token) = tokens.next() {
        let token = if matches!(token, "icmpv6" | "icmp6") {
            "ipv6-icmp"
        } else {
            token
        };
        rule.push(token.to_owned());
        if token == "-d"
            && let Some(destination) = tokens.next()
        {
            rule.push(normalize_destination(destination));
        }
    }
    rule.join(" ")
}

fn normalize_destination(destination: &str) -> String {
    let (addr, prefix) = match destination.split_once('/') {
        Some((addr, prefix)) => match prefix.parse::<u8>() {
            Ok(prefix) => (addr, Some(prefix)),
            Err(_) => return destination.to_owned(),
        },
        None => (destination, None),
    };
    let Ok(addr) = addr.parse::<IpAddr>() else {
        return destination.to_owned();
    };
    let full = if addr.is_ipv4() { 32 } else { 128 };
    format!("{addr}/{}", prefix.unwrap_or(full))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firewall_normalizes_kernel_host_addresses_and_ipv6_spelling() {
        assert_eq!(
            normalize("  -A echo_out -d 2001:0DB8::0053 -p udp  -m udp --dport 53 -j RETURN"), // lint-rules: allow-literal inert filter input, parsed only
            "-A echo_out -d 2001:db8::53/128 -p udp -m udp --dport 53 -j RETURN" // lint-rules: allow-literal inert filter output, compared only
        );
        assert_eq!(
            normalize("-A echo_out -d 192.0.2.53 -j RETURN"),
            "-A echo_out -d 192.0.2.53/32 -j RETURN"
        );
        assert_eq!(normalize("-A OUTPUT -o lo -j ACCEPT"), "-A OUTPUT -o lo -j ACCEPT");
    }
}
