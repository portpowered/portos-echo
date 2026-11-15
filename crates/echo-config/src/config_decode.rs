use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use toml::{
    Spanned,
    de::{DeValue as Value, ValueDeserializer},
};

use crate::{AgentConfig, AuditConfig, BridgeConfig, ConfigError, EchoConfig, LiveConfig, LiveMode};
use crate::{McpConfig, McpServer, ScreenConfig};

// Decode one field at a time so serde's input-bearing diagnostics never escape.
pub(super) struct Fields<'a> {
    table: BTreeMap<String, Spanned<Value<'a>>>,
    path: String,
}

impl<'a> Fields<'a> {
    pub(super) fn new(value: Value<'a>, path: &str) -> Result<Self, ConfigError> {
        match value {
            Value::Table(table) => Ok(Self {
                table: table
                    .into_iter()
                    .map(|(k, v)| (k.into_inner().into_owned(), v))
                    .collect(),
                path: path.into(),
            }),
            _ => Err(ConfigError::new(path, "expected table")),
        }
    }

    fn field(&self, key: &str) -> String {
        if self.path == "config" {
            key.into()
        } else {
            format!("{}.{key}", self.path)
        }
    }

    pub(super) fn optional<T: DeserializeOwned>(&mut self, key: &str) -> Result<Option<T>, ConfigError> {
        self.table
            .remove(key)
            .map(|value| {
                T::deserialize(ValueDeserializer::from(value))
                    .map_err(|_| ConfigError::new(self.field(key), "invalid type or value"))
            })
            .transpose()
    }

    fn required<T: DeserializeOwned>(&mut self, key: &str) -> Result<T, ConfigError> {
        self.optional(key)?
            .ok_or_else(|| ConfigError::new(self.field(key), "missing required field"))
    }

    fn node(&mut self, key: &str) -> Result<Value<'a>, ConfigError> {
        self.table
            .remove(key)
            .map(Spanned::into_inner)
            .ok_or_else(|| ConfigError::new(self.field(key), "missing required field"))
    }

    pub(super) fn finish(self) -> Result<(), ConfigError> {
        if self.table.is_empty() {
            Ok(())
        } else {
            // Unknown names are untrusted values too: name only the enclosing known field.
            Err(ConfigError::new(self.path, "unknown field"))
        }
    }
}

pub(super) fn decode(value: Value<'_>) -> Result<BridgeConfig, ConfigError> {
    let mut root = Fields::new(value, "config")?;
    let result = BridgeConfig {
        chatgpt_auth: root
            .table
            .remove("chatgpt_auth")
            .map(|v| crate::config_auth::decode(v.into_inner()))
            .transpose()?
            .unwrap_or_default(),
        echo: echo(root.node("echo")?)?,
        live: live(root.node("live")?)?,
        agent: agent(root.node("agent")?)?,
        mcp: mcp(root.node("mcp")?)?,
        screen: screen(root.node("screen")?)?,
        audit: audit(root.node("audit")?)?,
    };
    root.finish()?;
    Ok(result)
}

fn echo(value: Value<'_>) -> Result<EchoConfig, ConfigError> {
    let mut f = Fields::new(value, "echo")?;
    let result = EchoConfig {
        host: f.required("host")?,
        wyoming_port: f.optional("wyoming_port")?.unwrap_or(16700),
    };
    f.finish()?;
    Ok(result)
}

fn live(value: Value<'_>) -> Result<LiveConfig, ConfigError> {
    let mut f = Fields::new(value, "live")?;
    let result = LiveConfig {
        mode: f.optional("mode")?.unwrap_or(LiveMode::Chatgpt),
        credential_file: f.required("credential_file")?,
        voice: f.optional("voice")?,
        session_idle_seconds: f.optional("session_idle_seconds")?.unwrap_or(120),
    };
    f.finish()?;
    Ok(result)
}

fn agent(value: Value<'_>) -> Result<AgentConfig, ConfigError> {
    let mut f = Fields::new(value, "agent")?;
    let result = AgentConfig {
        max_tool_calls: f.optional("max_tool_calls")?.unwrap_or(8),
        task_deadline_seconds: f.optional("task_deadline_seconds")?.unwrap_or(60),
        daily_request_cap: f.optional("daily_request_cap")?.unwrap_or(500),
        model: f.optional("model")?,
    };
    f.finish()?;
    Ok(result)
}

fn mcp(value: Value<'_>) -> Result<McpConfig, ConfigError> {
    let mut f = Fields::new(value, "mcp")?;
    let values = match f.table.remove("server").map(Spanned::into_inner) {
        None => Vec::new(),
        Some(Value::Array(values)) => values.into_iter().collect(),
        Some(_) => return Err(ConfigError::new("mcp.server", "expected array")),
    };
    let server = values
        .into_iter()
        .enumerate()
        .map(|(i, value)| server(value.into_inner(), i))
        .collect::<Result<_, _>>()?;
    f.finish()?;
    Ok(McpConfig { server })
}

fn server(value: Value<'_>, index: usize) -> Result<McpServer, ConfigError> {
    let mut f = Fields::new(value, &format!("mcp.server[{index}]"))?;
    let result = McpServer {
        name: f.required("name")?,
        url: f.required("url")?,
        auth: f.required("auth")?,
        bearer_file: f.optional("bearer_file")?,
        allow_tools: f.optional("allow_tools")?.unwrap_or_default(),
    };
    f.finish()?;
    Ok(result)
}

fn screen(value: Value<'_>) -> Result<ScreenConfig, ConfigError> {
    let mut f = Fields::new(value, "screen")?;
    let listen: String = f.optional("listen")?.unwrap_or_else(|| "0.0.0.0:16900".into());
    let result = ScreenConfig {
        listen: listen
            .parse()
            .map_err(|_| ConfigError::new("screen.listen", "expected socket address"))?,
        pairing_token_file: f.required("pairing_token_file")?,
    };
    f.finish()?;
    Ok(result)
}

fn audit(value: Value<'_>) -> Result<AuditConfig, ConfigError> {
    let mut f = Fields::new(value, "audit")?;
    let result = AuditConfig {
        log_dir: f.required("log_dir")?,
    };
    f.finish()?;
    Ok(result)
}
