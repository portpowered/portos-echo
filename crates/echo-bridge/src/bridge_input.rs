//! Offline startup validation and explicitly nominated credential acquisition.
use crate::{Error, bridge_voice::RunnerSkills};
use echo_contracts::secret::Secret;
use echo_live::call::{CallError, CredentialProvider, LiveAuthMode, LiveCredential};
use echo_wyoming::Endpoint;
use std::{
    ffi::OsString,
    net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs},
    path::PathBuf,
};

pub struct Settings {
    pub config: Option<echo_config::BridgeConfig>,
    pub startup: Option<Box<echo_config::EffectiveConfig>>,
    pub runner_skills: RunnerSkills,
    pub settings_warning: Option<String>,
    pub sim_clock: bool,
    pub sim_epoch: Option<u64>,
    pub mcp: Option<crate::bridge_mcp::Settings>,
    pub kill_path: Option<PathBuf>,
    pub delegation: Option<(echo_config::AgentConfig, echo_config::McpServer)>,
    pub screen: Option<echo_config::ScreenConfig>,
    pub wyoming: Endpoint,
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
    parse_with_lab_socket(args, lab, None)
}

/// Admits a Unix path override in a complete loopback lab environment.
/// # Errors
/// Invalid argv, an invalid socket path or a socket override outside lab mode.
pub fn parse_with_lab_socket(
    args: &[String],
    lab: [Option<String>; 3],
    lab_socket: Option<OsString>,
) -> Result<Option<Settings>, Error> {
    if args == ["--help"] && lab_socket.is_none() {
        return Ok(None);
    }
    let lab_requested = lab_socket.is_some() || lab.iter().any(Option::is_some);
    let sim_clock = args.iter().filter(|arg| *arg == "--sim-clock").count();
    if sim_clock > 1 || (sim_clock != 0 && lab_requested) {
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
    if lab_requested {
        if config.is_some() || trace.is_some() || live.is_some() {
            return Err(Error::Input);
        }
        return lab_settings(lab, health.as_deref(), lab_socket).map(Some);
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

fn lab_settings(lab: [Option<String>; 3], health: Option<&str>, socket: Option<OsString>) -> Result<Settings, Error> {
    let [Some(wyoming), Some(url), Some(path)] = lab else {
        return Err(Error::Input);
    };
    let tcp = loopback(&wyoming, false)?;
    let wyoming = match socket {
        Some(path) => Endpoint::unix_first(path.into(), tcp).map_err(|_| Error::Input)?,
        None => tcp.into(),
    };
    Ok(Settings {
        config: None,
        startup: None,
        runner_skills: RunnerSkills::default(),
        settings_warning: None,
        sim_clock: false,
        sim_epoch: None,
        mcp: None,
        kill_path: None,
        delegation: None,
        screen: None,
        wyoming,
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
    let effective = echo_config::BridgeConfig::load_effective(config).map_err(|error| prompt_config_error(&error))?;
    let config = effective.config.clone();
    let runner_skills = RunnerSkills::load(&config)?;
    let mode = match config.live.mode {
        echo_config::LiveMode::Chatgpt => LiveAuthMode::Chatgpt,
        echo_config::LiveMode::ApiKey => LiveAuthMode::ApiKey,
    };
    let (live_url, model_url) = configured_origins(&config, origins)?;
    let credential = if origins.is_none() || config.codex.home.is_some() {
        Provider::Unavailable
    } else {
        Provider::Resolved(
            echo_signin::resolve_mode(config.live.clone(), config.chatgpt_auth.clone()).map_err(|_| Error::Input)?,
        )
    };
    Ok(Some(Settings {
        config: Some(config.clone()),
        settings_warning: effective.warning.as_ref().map(ToString::to_string),
        startup: Some(Box::new(effective)),
        runner_skills,
        sim_clock: false,
        sim_epoch: None,
        mcp: crate::bridge_mcp::Settings::configured(&config)?,
        kill_path: Some(config.audit.log_dir.join("bridge-kill")),
        delegation: config.mcp.server.first().cloned().map(|server| (config.agent, server)),
        screen: Some(config.screen),
        wyoming: echo_endpoint(
            echo_address(&config.echo.host, config.echo.wyoming_port)?,
            cfg!(target_os = "android"),
        )?,
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

fn echo_endpoint(address: SocketAddr, android: bool) -> Result<Endpoint, Error> {
    if android && address == SocketAddr::from((Ipv4Addr::LOCALHOST, 16700)) {
        Endpoint::unix_first(echo_device::WYOMING_SOCKET.into(), address).map_err(|_| Error::Input)
    } else {
        Ok(address.into())
    }
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
    // Only the retained legacy path consumes this endpoint. Codex refresh uses
    // the owned child's explicit LocalRoutes, never legacy auth configuration.
    if config.live.mode == echo_config::LiveMode::Chatgpt && config.codex.home.is_none() {
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

fn prompt_config_error(error: &echo_config::ConfigError) -> Error {
    let text = error.to_string();
    if [
        "home:",
        "home.dir:",
        "codex.instructions:",
        "codex.live_instructions:",
        "codex.web_search:",
        "codex.skills:",
        "codex.skills[",
        "agent.backend: removed;",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
    {
        Error::PromptConfig(text)
    } else {
        Error::Input
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_policy_selects_only_the_android_loopback_profile() {
        let local: SocketAddr = "127.0.0.1:16700".parse().unwrap();
        let remote: SocketAddr = "192.0.2.1:16700".parse().unwrap();
        let alternate: SocketAddr = "127.0.0.1:16701".parse().unwrap();
        let ipv6: SocketAddr = "[::1]:16700".parse().unwrap();
        assert_eq!(
            echo_endpoint(local, true).unwrap(),
            Endpoint::unix_first(echo_device::WYOMING_SOCKET.into(), local).unwrap()
        );
        for (address, android) in [(local, false), (remote, true), (alternate, true), (ipv6, true)] {
            assert_eq!(echo_endpoint(address, android).unwrap(), address.into());
        }
    }

    #[test]
    fn socket_policy_requires_complete_lab_inputs() {
        let lab = [
            Some("127.0.0.1:1".into()),
            Some("http://127.0.0.1:2".into()),
            Some("unused".into()),
        ];
        let path = "/tmp/satellite.sock";
        let selected = parse_with_lab_socket(&[], lab.clone(), Some(path.into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            selected.wyoming,
            Endpoint::unix_first(path.into(), "127.0.0.1:1".parse().unwrap()).unwrap()
        );
        let configured = ["--config".into(), "unused".into(), "--trace".into(), "unused".into()];
        for (args, env, path) in [
            (&[][..], lab, "relative"),
            (&[][..], [None, None, None], path),
            (&configured[..], [None, None, None], path),
        ] {
            assert_eq!(
                parse_with_lab_socket(args, env, Some(path.into())).err(),
                Some(Error::Input)
            );
        }
    }

    #[test]
    fn settings_overlay_configured_admission_uses_values_and_preserves_base() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("agent-home");
        std::fs::create_dir(&home).unwrap();
        let path = directory.path().join("bridge.toml");
        let base = format!(
            "{}\n[home]\ndir='{}'\n",
            include_str!("../../../examples/bridge.example.toml").replace("echo.example.invalid", "127.0.0.1"),
            home.display()
        );
        std::fs::write(&path, &base).unwrap();
        let settings = home.join("settings.toml");
        std::fs::write(&settings, "[voice]\nsession_listen_enabled=false\nsession_listen_seconds=42\n[codex]\nvoice='ember'\n[agent]\ndaily_request_cap=0\nmax_tool_calls=3\n").unwrap();
        let loaded = configured(path.to_str().unwrap().into(), "unused-trace".into(), None, None)
            .unwrap()
            .unwrap();
        let config = loaded.config.unwrap();
        assert_eq!(config.voice.session_listen_seconds, 42);
        assert!(!config.voice.session_listen_enabled);
        assert_eq!(config.codex_voice(), Some("ember"));
        assert_eq!(config.agent.daily_request_cap, 0);
        assert_eq!(loaded.startup.as_ref().unwrap().overlay.legacy_tool_cap(), Some(3));
        assert_eq!(loaded.delegation.unwrap().0.daily_request_cap, 0);
        assert_eq!(loaded.settings_warning, None);
        std::fs::write(
            &settings,
            "[voice]\nsession_listen_seconds=42\n[agent]\nmax_tool_calls=0\n",
        )
        .unwrap();
        let loaded = configured(path.to_str().unwrap().into(), "unused-trace".into(), None, None)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.config.unwrap().voice.session_listen_seconds, 15);
        assert_eq!(loaded.delegation.unwrap().0.max_tool_calls, 8);
        assert_eq!(
            loaded.settings_warning.as_deref(),
            Some("settings.toml: ignored overlay: agent.max_tool_calls: must be within 1..=32")
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), base);
        assert!(!directory.path().join("unused-trace").exists());
    }

    /// Owner, 2026-11-20: a saved `alloy` failed every wake at the v3 start.
    #[test]
    fn settings_overlay_stored_retired_voice_starts_as_default_with_one_warning() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("agent-home");
        std::fs::create_dir(&home).unwrap();
        let path = directory.path().join("bridge.toml");
        let example =
            include_str!("../../../examples/bridge.example.toml").replace("echo.example.invalid", "127.0.0.1");
        std::fs::write(&path, format!("{example}\n[home]\ndir='{}'\n", home.display())).unwrap();
        let stored = "[voice]\nsession_listen_seconds=42\n[codex]\nvoice='alloy'\n";
        std::fs::write(home.join("settings.toml"), stored).unwrap();
        let loaded = configured(path.to_str().unwrap().into(), "unused-trace".into(), None, None)
            .unwrap()
            .unwrap();
        let config = loaded.config.unwrap();
        assert_eq!(config.codex_voice(), None);
        assert_eq!(config.voice.session_listen_seconds, 42);
        assert_eq!(
            loaded.settings_warning.as_deref(),
            Some("settings.toml: ignored codex.voice: not supported by the v3 realtime version; using default")
        );
        assert_eq!(std::fs::read_to_string(home.join("settings.toml")).unwrap(), stored);
    }

    #[test]
    fn canonical_local_routes_ignore_only_the_unused_legacy_refresh_endpoint() {
        let mut config = echo_config::BridgeConfig::load(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/bridge.example.toml"),
        )
        .unwrap();
        config.echo.host = "127.0.0.1".into();
        config.screen.listen = "127.0.0.1:0".parse().unwrap();
        config.mcp.server.clear();
        let origins = Some(("http://127.0.0.1:1", "http://127.0.0.1:2"));
        assert_eq!(configured_origins(&config, origins), Err(Error::Input));
        config.codex.home = Some(std::env::temp_dir().join("unused-codex-home"));
        assert_eq!(
            configured_origins(&config, origins),
            Ok(("http://127.0.0.1:1".into(), Some("http://127.0.0.1:2".into())))
        );
        config.echo.host = "non-loopback.invalid".into();
        assert_eq!(configured_origins(&config, origins), Err(Error::Input));
    }
}
