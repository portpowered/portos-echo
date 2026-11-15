use std::{collections::BTreeSet, net::Ipv6Addr};

use crate::{BridgeConfig, ConfigError, McpAuth};

pub(super) fn reject_secrets(value: &toml::de::DeValue<'_>, path: &str) -> Result<(), ConfigError> {
    match value {
        toml::de::DeValue::Table(table) => {
            for (key, child) in table {
                let key = key.get_ref();
                let field = if path.is_empty() {
                    key.to_string()
                } else {
                    format!("{path}.{key}")
                };
                if ["token", "secret", "password", "api_key"].contains(&key.as_ref()) {
                    return Err(ConfigError::new(field, "secrets go in files, not config"));
                }
                reject_secrets(child.get_ref(), &field)?;
            }
        }
        toml::de::DeValue::Array(array) => {
            for (index, child) in array.iter().enumerate() {
                reject_secrets(child.get_ref(), &format!("{path}[{index}]"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn validate(config: &BridgeConfig) -> Result<(), ConfigError> {
    if config.echo.wyoming_port == 0 {
        return Err(ConfigError::new("echo.wyoming_port", "must be nonzero"));
    }
    if !(1..=32).contains(&config.agent.max_tool_calls) {
        return Err(ConfigError::new("agent.max_tool_calls", "must be within 1..=32"));
    }
    if !(5..=600).contains(&config.agent.task_deadline_seconds) {
        return Err(ConfigError::new(
            "agent.task_deadline_seconds",
            "must be within 5..=600",
        ));
    }
    let mut names = BTreeSet::new();
    for (index, server) in config.mcp.server.iter().enumerate() {
        let path = format!("mcp.server[{index}]");
        if !names.insert(&server.name) {
            return Err(ConfigError::new(format!("{path}.name"), "duplicate server name"));
        }
        if !http_url(&server.url) {
            return Err(ConfigError::new(
                format!("{path}.url"),
                "expected HTTP(S) URL without inline credentials",
            ));
        }
        if server.auth == McpAuth::BearerFile && server.bearer_file.is_none() {
            return Err(ConfigError::new(
                format!("{path}.bearer_file"),
                "required for bearer_file auth",
            ));
        }
    }
    Ok(())
}

// Deliberately offline: validate authority syntax without DNS or a network dependency.
fn http_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        || url.chars().any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return false;
    }
    let (host, port) = if let Some(ipv6) = authority.strip_prefix('[') {
        let Some((address, suffix)) = ipv6.split_once(']') else {
            return false;
        };
        if address.parse::<Ipv6Addr>().is_err() {
            return false;
        }
        (address, suffix)
    } else {
        let end = authority.find(':').unwrap_or(authority.len());
        let host = &authority[..end];
        if !host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') {
            return false;
        }
        (host, &authority[end..])
    };
    !host.is_empty() && (port.is_empty() || port.strip_prefix(':').is_some_and(|p| p.parse::<u16>().is_ok()))
}
