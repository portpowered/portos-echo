//! Offline startup validation and explicitly nominated credential acquisition.
use crate::Error;
use echo_contracts::secret::Secret;
use echo_live::call::{CallError, CredentialProvider, LiveAuthMode, LiveCredential};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs},
    path::PathBuf,
};

pub struct Settings {
    pub config: Option<echo_config::BridgeConfig>,
    pub sim_clock: bool,
    pub sim_epoch: Option<u64>,
    pub mcp: Option<crate::bridge_mcp::Settings>,
    pub kill_path: Option<PathBuf>,
    pub muse: Option<(echo_config::AgentConfig, echo_config::MuseConfig)>,
    pub delegation: Option<(echo_config::AgentConfig, echo_config::McpServer)>,
    pub screen: Option<echo_config::ScreenConfig>,
    pub wyoming: SocketAddr,
    pub live_url: String,
    pub model_url: Option<String>,
    pub trace: PathBuf,
    pub health: SocketAddr,
    pub mode: LiveAuthMode,
    pub voice: String,
    pub credential: Provider,
    pub lab: bool,
    pub reply_lead_ms: u16,
    pub session_idle_seconds: u64,
}

impl Settings {
    /// Selects a shorter local-fake idle interval without reading credentials.
    /// # Errors
    /// Rejects an override outside lab mode or an invalid unsigned duration.
    pub fn lab_idle(&mut self, seconds: Option<&str>) -> Result<(), Error> {
        if let Some(seconds) = seconds {
            if !self.lab || seconds.is_empty() || !seconds.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(Error::Input);
            }
            self.session_idle_seconds = seconds.parse().map_err(|_| Error::Input)?;
        }
        Ok(())
    }
}

#[derive(Clone)]
pub enum Provider {
    Synthetic,
    Resolved(echo_signin::Providers),
    ApiKey(PathBuf),
    Unavailable,
}

impl CredentialProvider for Provider {
    async fn credential(&self) -> Result<LiveCredential, CallError> {
        match self {
            Self::Resolved(providers) => providers.credential().await,
            _ => self.acquire(),
        }
    }
}

impl Provider {
    fn acquire(&self) -> Result<LiveCredential, CallError> {
        match self {
            Self::Synthetic => Ok(LiveCredential::Chatgpt {
                token: Secret::new("synthetic-local-only".into()),
                account_id: "synthetic".into(),
            }),
            Self::ApiKey(path) => {
                let file = std::fs::File::open(path).map_err(|_| CallError::Authorization)?;
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut std::io::Read::take(file, 16_385), &mut bytes)
                    .map_err(|_| CallError::Authorization)?;
                if bytes.len() > 16_384 {
                    return Err(CallError::Authorization);
                }
                let text = String::from_utf8(bytes).map_err(|_| CallError::Authorization)?;
                let key = text
                    .strip_suffix("\r\n")
                    .or_else(|| text.strip_suffix('\n'))
                    .unwrap_or(&text);
                if key.trim().is_empty() || key.chars().any(char::is_control) {
                    return Err(CallError::Authorization);
                }
                Ok(LiveCredential::ApiKey {
                    key: Secret::new(key.into()),
                })
            }
            Self::Unavailable | Self::Resolved(_) => Err(CallError::Authorization),
        }
    }
}

/// # Errors
/// Rejects hostnames/nonloopback inputs before any dial.
pub fn loopback(text: &str, ephemeral: bool) -> Result<SocketAddr, Error> {
    let address: SocketAddr = text.parse().map_err(|_| Error::Input)?;
    if address.ip() != Ipv4Addr::LOCALHOST || (!ephemeral && address.port() == 0) {
        return Err(Error::Input);
    }
    Ok(address)
}

/// No URL parser dependency: factory validates the origin again before use.
/// # Errors
/// Admits only a plain loopback fake origin with a nonzero numeric port.
pub fn lab_origin(text: &str) -> Result<String, Error> {
    let address = text.strip_prefix("http://").ok_or(Error::Input)?;
    let address = address.strip_suffix('/').unwrap_or(address);
    loopback(address, false)?;
    Ok(text.into())
}

/// # Errors
/// Invalid argv/configuration is an offline failure, with no credential read.
pub fn parse(args: &[String], lab: [Option<String>; 3]) -> Result<Option<Settings>, Error> {
    if args == ["--help"] {
        return Ok(None);
    }
    let sim_clock = args.iter().filter(|arg| *arg == "--sim-clock").count();
    if sim_clock > 1 || (sim_clock != 0 && lab.iter().any(Option::is_some)) {
        return Err(Error::Input);
    }
    let filtered: Vec<_> = args.iter().filter(|arg| *arg != "--sim-clock").cloned().collect();
    let args = &filtered;
    let mut config = None;
    let mut trace = None;
    let mut health = None;
    let mut live = None;
    let mut model = None;
    let mut epoch = None;
    let (pairs, remainder) = args.as_chunks::<2>();
    for pair in pairs {
        let target = match pair[0].as_str() {
            "--config" => &mut config,
            "--trace" => &mut trace,
            "--health-listen" => &mut health,
            "--live-loopback" => &mut live,
            "--model-loopback" => &mut model,
            "--sim-epoch" if sim_clock != 0 => &mut epoch,
            _ => return Err(Error::Input),
        };
        if target.replace(pair[1].clone()).is_some() {
            return Err(Error::Input);
        }
    }
    if !remainder.is_empty() || live.is_some() != model.is_some() || (sim_clock != 0 && live.is_none()) {
        return Err(Error::Input);
    }
    if lab.iter().any(Option::is_some) {
        if config.is_some() || trace.is_some() || live.is_some() {
            return Err(Error::Input);
        }
        return lab_settings(lab, health.as_deref()).map(Some);
    }
    let mut settings = configured(
        config.ok_or(Error::Input)?,
        trace.ok_or(Error::Input)?,
        health.as_deref(),
        live.as_deref().zip(model.as_deref()),
    )?;
    if let Some(settings) = &mut settings {
        settings.sim_clock = sim_clock != 0;
        settings.sim_epoch = epoch.map(|value| parse_epoch(&value)).transpose()?;
    }
    Ok(settings)
}

fn lab_settings(lab: [Option<String>; 3], health: Option<&str>) -> Result<Settings, Error> {
    let [Some(wyoming), Some(url), Some(path)] = lab else {
        return Err(Error::Input);
    };
    Ok(Settings {
        config: None,
        sim_clock: false,
        sim_epoch: None,
        mcp: None,
        kill_path: None,
        delegation: None,
        muse: None,
        screen: None,
        wyoming: loopback(&wyoming, false)?,
        live_url: lab_origin(&url)?,
        model_url: None,
        trace: path.into(),
        health: loopback(health.unwrap_or("127.0.0.1:0"), true)?,
        mode: LiveAuthMode::Chatgpt,
        voice: "alloy".into(),
        credential: Provider::Synthetic,
        lab: true,
        reply_lead_ms: 200,
        session_idle_seconds: 120,
    })
}

fn configured(
    config: String,
    trace: String,
    health: Option<&str>,
    origins: Option<(&str, &str)>,
) -> Result<Option<Settings>, Error> {
    let config = echo_config::BridgeConfig::load(config).map_err(|_| Error::Input)?;
    let muse = if config.agent_backend == echo_config::AgentBackend::Muse {
        Some((config.agent.clone(), config.muse.clone().ok_or(Error::Input)?))
    } else {
        None
    };
    let mode = match config.live.mode {
        echo_config::LiveMode::Chatgpt => LiveAuthMode::Chatgpt,
        echo_config::LiveMode::ApiKey => LiveAuthMode::ApiKey,
    };
    let (live_url, model_url) = configured_origins(&config, origins)?;
    let credential = if origins.is_none() && muse.is_none() {
        Provider::Unavailable
    } else {
        Provider::Resolved(
            echo_signin::resolve_mode(config.live.clone(), config.chatgpt_auth.clone()).map_err(|_| Error::Input)?,
        )
    };
    Ok(Some(Settings {
        config: Some(config.clone()),
        sim_clock: false,
        sim_epoch: None,
        mcp: if muse.is_none() {
            crate::bridge_mcp::Settings::configured(&config)?
        } else {
            None
        },
        muse,
        kill_path: Some(config.audit.log_dir.join("bridge-kill")),
        delegation: if config.agent_backend == echo_config::AgentBackend::Client {
            config.mcp.server.first().cloned().map(|server| (config.agent, server))
        } else {
            None
        },
        screen: Some(config.screen),
        wyoming: echo_address(&config.echo.host, config.echo.wyoming_port)?,
        live_url,
        model_url,
        trace: trace.into(),
        health: loopback(health.unwrap_or("127.0.0.1:16901"), true)?,
        mode,
        voice: config.live.voice.unwrap_or_else(|| "alloy".into()),
        credential,
        lab: false,
        reply_lead_ms: config.audio.reply_lead_ms,
        session_idle_seconds: config.live.session_idle_seconds,
    }))
}

/// The Echo's Wyoming address for a configured run: the bridge on the operator's PC dials the Echo across the LAN
/// (plan sections 1-2), so any IP literal is accepted, and a name is resolved once, here. Lab runs stay loopback-only
/// (`configured_origins`).
///
/// # Errors
/// A zero port, or a name that does not resolve.
pub fn echo_address(host: &str, port: u16) -> Result<SocketAddr, Error> {
    if port == 0 {
        return Err(Error::Input);
    }
    let literal = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    if let Ok(ip) = literal.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, port));
    }
    (host, port)
        .to_socket_addrs()
        .map_err(|_| Error::Input)?
        .min_by_key(SocketAddr::is_ipv6)
        .ok_or(Error::Input)
}

fn configured_origins(
    config: &echo_config::BridgeConfig,
    origins: Option<(&str, &str)>,
) -> Result<(String, Option<String>), Error> {
    let Some((live, model)) = origins else {
        let base = match config.live.mode {
            echo_config::LiveMode::Chatgpt => "https://chatgpt.com",
            echo_config::LiveMode::ApiKey => "https://api.openai.com",
        };
        return Ok((base.into(), None));
    };
    // Overrides must be origins, not endpoint URLs (including a trailing slash).
    for origin in [live, model] {
        let address = origin.strip_prefix("http://").ok_or(Error::Input)?;
        loopback(address, false)?;
    }
    loopback(&format!("{}:{}", config.echo.host, config.echo.wyoming_port), false)?;
    loopback(&config.screen.listen.to_string(), true)?;
    for server in &config.mcp.server {
        loopback_endpoint(&server.url)?;
    }
    // A configured run can refresh credentials, but never starts signin itself.
    if config.live.mode == echo_config::LiveMode::Chatgpt {
        loopback_endpoint(&config.chatgpt_auth.token_endpoint)?;
    }
    Ok((live.into(), Some(model.into())))
}

fn loopback_endpoint(url: &str) -> Result<(), Error> {
    let rest = url.strip_prefix("http://").ok_or(Error::Input)?;
    let authority = rest.split('/').next().ok_or(Error::Input)?;
    loopback(authority, false)?;
    Ok(())
}

fn parse_epoch(value: &str) -> Result<u64, Error> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::Input);
    }
    value.parse().map_err(|_| Error::Input)
}
