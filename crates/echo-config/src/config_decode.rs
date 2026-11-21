use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use toml::{
    Spanned,
    de::{DeValue as Value, ValueDeserializer},
};

use crate::{AgentConfig, AuditConfig, BridgeConfig, ConfigError, EchoConfig, LiveConfig, LiveMode};
use crate::{McpConfig, McpServer};

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

    pub(super) fn required<T: DeserializeOwned>(&mut self, key: &str) -> Result<T, ConfigError> {
        self.optional(key)?
            .ok_or_else(|| ConfigError::new(self.field(key), "missing required field"))
    }

    pub(super) fn section(&mut self, key: &str) -> Result<Option<Self>, ConfigError> {
        self.table
            .remove(key)
            .map(|value| Self::new(value.into_inner(), &self.field(key)))
            .transpose()
    }

    pub(super) fn discard(&mut self, key: &str) -> bool {
        self.table.remove(key).is_some()
    }

    // Decode stored compatibility metadata alongside the supported settings.
    pub(super) fn compatibility_value<T: DeserializeOwned>(&mut self, key: &str) -> Result<Option<T>, ConfigError> {
        self.optional(key)
    }

    pub(super) fn reject(&self, key: &str, message: &str) -> Result<(), ConfigError> {
        if self.table.contains_key(key) {
            return Err(ConfigError::new(self.field(key), message));
        }
        Ok(())
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

pub(super) fn decode(value: Value<'_>) -> Result<(BridgeConfig, Vec<ConfigError>), ConfigError> {
    let mut root = Fields::new(value, "config")?;
    let home = root.table.remove("home").map(|v| home(v.into_inner())).transpose()?;
    let mut warnings = Vec::new();
    let result = BridgeConfig {
        home,
        wake_earcon: root.optional("wake_earcon")?.unwrap_or(true),
        codex: root
            .table
            .remove("codex")
            .map(|v| crate::config_codex::decode(v.into_inner()))
            .transpose()?
            .unwrap_or_default(),
        chatgpt_auth: root
            .table
            .remove("chatgpt_auth")
            .map(|v| crate::config_auth::decode(v.into_inner()))
            .transpose()?
            .unwrap_or_default(),
        echo: echo(root.node("echo")?)?,
        live: live(root.node("live")?)?,
        voice: root
            .table
            .remove("voice")
            .map(|v| crate::config_voice::decode(v.into_inner()))
            .transpose()?
            .unwrap_or_default(),
        audio: root
            .table
            .remove("audio")
            .map(|v| audio(v.into_inner()))
            .transpose()?
            .unwrap_or_default(),
        clock: crate::config_clock::decode(root.table.remove("clock").map(Spanned::into_inner))?,
        agent: agent(root.node("agent")?, &mut warnings)?,
        muse: root
            .table
            .remove("muse")
            .map(|v| crate::config_muse::decode(v.into_inner()))
            .transpose()?,
        mcp: mcp(root.node("mcp")?)?,
        screen: crate::config_screen::decode(root.node("screen")?)?,
        audit: audit(root.node("audit")?)?,
        trace: root
            .table
            .remove("trace")
            .map(|v| trace(v.into_inner()))
            .transpose()?
            .unwrap_or_default(),
    };
    root.finish()?;
    Ok((result, warnings))
}

fn trace(value: Value<'_>) -> Result<crate::TraceConfig, ConfigError> {
    let mut fields = Fields::new(value, "trace")?;
    let result = crate::TraceConfig {
        payloads: fields.optional("payloads")?.unwrap_or(true),
    };
    fields.finish()?;
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

fn audio(value: Value<'_>) -> Result<crate::AudioConfig, ConfigError> {
    let mut f = Fields::new(value, "audio")?;
    let result = crate::AudioConfig {
        reply_lead_ms: f.optional("reply_lead_ms")?.unwrap_or(200),
    };
    f.finish()?;
    Ok(result)
}

fn agent(value: Value<'_>, warnings: &mut Vec<ConfigError>) -> Result<AgentConfig, ConfigError> {
    let mut f = Fields::new(value, "agent")?;
    for (present, field) in [
        (f.discard("max_tool_calls"), "agent.max_tool_calls"),
        (f.discard("task_deadline_seconds"), "agent.task_deadline_seconds"),
    ] {
        if present {
            warnings.push(ConfigError::new(field, "deprecated and ignored"));
        }
    }
    let result = AgentConfig {
        max_tool_calls: 8,
        task_deadline_seconds: 60,
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
    let defaults = McpConfig::default();
    let listen: String = f
        .optional("operator_listen")?
        .unwrap_or_else(|| defaults.operator_listen.to_string());
    let result = McpConfig {
        server,
        state_dir: f.optional("state_dir")?,
        client_id: f.optional("client_id")?,
        client_name: f.optional("client_name")?.unwrap_or(defaults.client_name),
        scopes: f.optional("scopes")?.unwrap_or(defaults.scopes),
        operator_listen: listen
            .parse()
            .map_err(|_| ConfigError::new("mcp.operator_listen", "expected socket address"))?,
    };
    f.finish()?;
    Ok(result)
}

fn server(value: Value<'_>, index: usize) -> Result<McpServer, ConfigError> {
    let mut f = Fields::new(value, &format!("mcp.server[{index}]"))?;
    let auth: crate::McpAuth = f.required("auth")?;
    let result = McpServer {
        name: f.required("name")?,
        url: f.required("url")?,
        auth,
        bearer_file: f.optional("bearer_file")?,
        allow_tools: f.optional("allow_tools")?.unwrap_or_else(|| auth.default_tools()),
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

fn home(value: Value<'_>) -> Result<crate::HomeConfig, ConfigError> {
    let mut fields = Fields::new(value, "home")?;
    let dir: std::path::PathBuf = fields.required("dir")?;
    let agent_tools = agent_tools(fields.optional("agent_tools")?)?;
    fields.finish()?;
    if !dir.is_absolute() {
        return Err(ConfigError::new("home.dir", "expected nonempty absolute path"));
    }
    Ok(crate::HomeConfig { dir, agent_tools })
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum AgentToolsSelection {
    Enabled(bool),
    Groups(Vec<AgentToolGroup>),
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum AgentToolGroup {
    Notes,
    Prompts,
}

fn agent_tools(selection: Option<AgentToolsSelection>) -> Result<crate::AgentTools, ConfigError> {
    let mut result = crate::AgentTools {
        notes: false,
        prompts: false,
    };
    match selection {
        None | Some(AgentToolsSelection::Enabled(true)) => return Ok(crate::AgentTools::default()),
        Some(AgentToolsSelection::Enabled(false)) => {}
        Some(AgentToolsSelection::Groups(groups)) => {
            for group in groups {
                let enabled = match group {
                    AgentToolGroup::Notes => &mut result.notes,
                    AgentToolGroup::Prompts => &mut result.prompts,
                };
                if *enabled {
                    return Err(ConfigError::new("home.agent_tools", "duplicate group"));
                }
                *enabled = true;
            }
        }
    }
    Ok(result)
}
