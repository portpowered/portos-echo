//! Athena's model rules and confined domain are derived from the pinned
//! echo-dot-assist installer (7e0c758, MIT, see third_party/hassmic/LICENSE).

use std::path::Path;

use echo_boot::Result;

use super::files::{self, Files, ROOT};

pub(super) const PATH: &str = "/vendor/etc/selinux/precompiled_sepolicy";
#[cfg(target_os = "android")]
pub(super) const SHA256: &str = "e5fafc1fa9486950ce9ba476f5723754d260014515ced72b4560238e6c560f3a";
const MODEL_RULES: &str = "\
allow init su process { transition siginh rlimitinh noatsecure }
allow su shell_exec file { read getattr map execute entrypoint open }
allow su system_file file { read getattr map execute entrypoint open }
allow su su process setcurrent
allow domain su binder { call transfer }
allow domain su fd use
allow domain su process sigchld
allow domain su fifo_file { read write getattr ioctl append lock map open }
allow domain su file { read write getattr ioctl append lock map open }
allow domain su unix_stream_socket { connectto read write getattr ioctl shutdown }
allow netd su tcp_socket { read write getattr getopt setopt ioctl }
allow netd su udp_socket { read write getattr getopt setopt ioctl }
allow netd su rawip_socket { read write getattr getopt setopt ioctl }
";

const DOMAIN_RULES: &str = "\
type hassmic domain
allow hassmic * * *
deny hassmic * property_service *
deny hassmic * process { transition dyntransition setcurrent setexec ptrace }
deny hassmic * { file dir lnk_file sock_file fifo_file chr_file blk_file } { relabelfrom relabelto }
deny hassmic * service_manager { add }
deny hassmic * binder { set_context_mgr }
deny hassmic * security { setenforce load_policy setbool setsecparam setcheckreqprot read_policy }
allow su hassmic process { dyntransition transition siginh rlimitinh noatsecure }
";

pub(super) trait Tool {
    fn policy(&mut self, tool: &Path, args: &[&Path], print: bool) -> Result<String>;
    fn verify_policy_tool(&self, files: &Files<'_>, tool: &Path) -> Result<()>;
}

pub(super) fn prepare(files: &Files<'_>, ops: &mut impl Tool, tool: &Path, work: &Path) -> Result<()> {
    let input = files.path(PATH);
    let model = work.join("model.rules");
    let intermediate = work.join("policy.model");
    files.write(&model, MODEL_RULES.as_bytes(), ROOT, 0o600)?;
    apply(ops, tool, &input, &model, &intermediate)?;
    let rules = ops.policy(
        tool,
        &[Path::new("--load"), &intermediate, Path::new("--print-rules")],
        true,
    )?;
    let output = work.join("policy.next");
    if has_type(&rules) {
        confined(&rules)?;
        files::rename(&intermediate, &output)?;
    } else {
        let domain = work.join("domain.rules");
        let rules = inbound_rules(&rules)?;
        files.write(&domain, rules.as_bytes(), ROOT, 0o600)?;
        apply(ops, tool, &intermediate, &domain, &output)?;
        let result = ops.policy(tool, &[Path::new("--load"), &output, Path::new("--print-rules")], true)?;
        confined(&result)?;
    }
    if files
        .trusted_file(&output)?
        .metadata()
        .map_err(|_| "inspect compiled policy")?
        .len()
        == 0
    {
        return Err("empty compiled policy");
    }
    Ok(())
}

fn apply(ops: &mut impl Tool, tool: &Path, source: &Path, rules: &Path, output: &Path) -> Result<()> {
    let _output = ops.policy(
        tool,
        &[
            Path::new("--load"),
            source,
            Path::new("--apply"),
            rules,
            Path::new("--save"),
            output,
        ],
        false,
    )?;
    Ok(())
}

fn has_type(rules: &str) -> bool {
    rules.lines().any(|line| line.starts_with("type hassmic "))
}

fn confined(rules: &str) -> Result<()> {
    if !has_type(rules) || !rules.lines().any(|line| line.starts_with("allow init su process ")) {
        return Err("policy lacks A2 boot domains");
    }
    for line in rules.lines() {
        let parts: Vec<_> = line.split_ascii_whitespace().collect();
        match parts.as_slice() {
            ["permissive", "hassmic"] | ["allow", "hassmic", _, "property_service", ..] => {
                return Err("policy does not confine hassmic");
            }
            ["allow", "hassmic", _, "process", permissions @ ..]
                if permissions
                    .iter()
                    .any(|word| matches!(*word, "dyntransition" | "setcurrent" | "setexec" | "ptrace")) =>
            {
                return Err("policy does not confine hassmic");
            }
            _ => (),
        }
    }
    Ok(())
}

fn inbound_rules(rules: &str) -> Result<String> {
    use std::fmt::Write as _;
    let mut output = DOMAIN_RULES.to_owned();
    let permissive: std::collections::BTreeSet<_> = rules
        .lines()
        .filter_map(|line| line.strip_prefix("permissive "))
        .collect();
    for line in rules.lines() {
        let mut parts = line.splitn(4, ' ');
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some("type"), Some("su"), Some("{"), Some(attributes)) => {
                writeln!(output, "typeattribute hassmic {{ {attributes}").map_err(|_| "encode domain rules")?;
            }
            (Some(verb @ ("allow" | "allowxperm")), Some(source), Some("su"), Some(rest))
                if source != "su" && valid_name(source) =>
            {
                writeln!(output, "{verb} {source} hassmic {rest}").map_err(|_| "encode domain rules")?;
            }
            (Some("type_transition"), Some("su"), Some(executable), Some(rest)) => {
                if let Some(domain) = rest
                    .strip_prefix("process ")
                    .filter(|name| valid_name(name) && !permissive.contains(name))
                {
                    writeln!(output, "type_transition hassmic {executable} process {domain}\nallow hassmic {domain} process {{ transition siginh rlimitinh noatsecure }}")
                        .map_err(|_| "encode domain transition")?;
                }
            }
            _ => (),
        }
    }
    Ok(output)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}
