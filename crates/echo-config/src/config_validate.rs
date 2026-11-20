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
                if schema_field(path, key) {
                    reject_secrets(child.get_ref(), &field)?;
                } else if let Some(secret) = forbidden_key(child.get_ref()) {
                    let field = if path.is_empty() {
                        secret.into()
                    } else {
                        format!("{path}.{secret}")
                    };
                    return Err(ConfigError::new(field, "secrets go in files, not config"));
                }
            }
        }
        toml::de::DeValue::Array(array) if path == "mcp.server" => {
            for (index, child) in array.iter().enumerate() {
                reject_secrets(child.get_ref(), &format!("{path}[{index}]"))?;
            }
        }
        _ => {
            if let Some(secret) = forbidden_key(value) {
                return Err(ConfigError::new(
                    format!("{path}.{secret}"),
                    "secrets go in files, not config",
                ));
            }
        }
    }
    Ok(())
}

// Only schema-defined ancestors may enter a diagnostic. Unknown subtrees are
// still scanned, but their keys and array positions are rejected input values.
fn schema_field(path: &str, key: &str) -> bool {
    let fields: &[&str] = match path {
        "" => &[
            "home",
            "echo",
            "live",
            "voice",
            "audio",
            "clock",
            "agent",
            "muse",
            "mcp",
            "screen",
            "audit",
            "chatgpt_auth",
            "codex",
        ],
        "home" => &["dir"],
        "audio" => &["reply_lead_ms"],
        "clock" => &["servers", "interval_seconds"],
        "voice" => &[
            "session_listen_enabled",
            "session_listen_seconds",
            "session_max_seconds",
        ],
        "codex" => &["home", "binary", "launch", "ca_file", "voice", "instructions", "model"],
        "chatgpt_auth" => crate::config_auth::FIELDS,
        "echo" => &["host", "wyoming_port"],
        "live" => &["mode", "credential_file", "voice", "session_idle_seconds"],
        "agent" => &[
            "backend",
            "max_tool_calls",
            "task_deadline_seconds",
            "daily_request_cap",
            "model",
        ],
        "muse" => &["url", "ca_file", "credential_file"],
        "mcp" => &["server", "state_dir", "client_id", "scopes", "operator_listen"],
        "screen" => &["listen", "pairing_token_file", "packs"],
        "audit" => &["log_dir"],
        p if p.starts_with("mcp.server[") && p.ends_with(']') => &["name", "url", "auth", "bearer_file", "allow_tools"],
        _ => &[],
    };
    fields.contains(&key)
}

fn forbidden_key(value: &toml::de::DeValue<'_>) -> Option<&'static str> {
    match value {
        toml::de::DeValue::Table(table) => table.iter().find_map(|(key, child)| {
            ["token", "secret", "password", "api_key"]
                .into_iter()
                .find(|secret| key.get_ref() == secret)
                .or_else(|| forbidden_key(child.get_ref()))
        }),
        toml::de::DeValue::Array(array) => array.iter().find_map(|child| forbidden_key(child.get_ref())),
        _ => None,
    }
}

pub(super) fn validate(config: &BridgeConfig) -> Result<(), ConfigError> {
    crate::config_screen::validate(config)?;
    config.voice.validate()?;
    crate::config_codex::validate(&config.codex)?;
    crate::config_codex::validate_voice(config)?;
    crate::config_muse::validate(config)?;
    config.chatgpt_auth.validate()?;
    validate_mcp(&config.mcp)?;
    crate::config_clock::validate(&config.clock)?;
    if config.audio.reply_lead_ms == 0 || !config.audio.reply_lead_ms.is_multiple_of(20) {
        return Err(ConfigError::new(
            "audio.reply_lead_ms",
            "must be a positive multiple of 20",
        ));
    }
    if config.echo.wyoming_port == 0 {
        return Err(ConfigError::new("echo.wyoming_port", "must be nonzero"));
    }
    if config.agent.model.as_ref().is_some_and(|model| model.trim().is_empty()) {
        return Err(ConfigError::new("agent.model", "must be nonblank"));
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

fn validate_mcp(config: &crate::McpConfig) -> Result<(), ConfigError> {
    if config.operator_listen.ip() != std::net::Ipv4Addr::LOCALHOST {
        return Err(ConfigError::new(
            "mcp.operator_listen",
            "expected literal IPv4 loopback",
        ));
    }
    if config.client_id.trim().is_empty() || config.client_id.chars().any(char::is_control) {
        return Err(ConfigError::new("mcp.client_id", "expected nonblank text"));
    }
    if config.scopes.is_empty()
        || config
            .scopes
            .iter()
            .any(|scope| scope.is_empty() || scope.chars().any(|c| c.is_whitespace() || c.is_control()))
    {
        return Err(ConfigError::new("mcp.scopes", "expected nonempty scope names"));
    }
    if config
        .state_dir
        .as_ref()
        .is_some_and(|path| path.as_os_str().is_empty())
    {
        return Err(ConfigError::new("mcp.state_dir", "expected nonempty path"));
    }
    Ok(())
}

// Deliberately offline: validate authority syntax without DNS or a network dependency.
pub(super) fn http_url(url: &str) -> bool {
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
