//! Offline startup validation and explicitly nominated credential acquisition.
use crate::Error;
use echo_contracts::secret::Secret;
use echo_live::call::{CallError, CredentialProvider, LiveAuthMode, LiveCredential};
use std::{
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
};

pub struct Settings {
    pub wyoming: SocketAddr,
    pub live_url: String,
    pub trace: PathBuf,
    pub health: SocketAddr,
    pub mode: LiveAuthMode,
    pub voice: String,
    pub credential: Provider,
    pub lab: bool,
    pub reply_lead_ms: u16,
    pub session_idle_seconds: u64,
}

pub enum Provider {
    Synthetic,
    ApiKey(PathBuf),
    Unavailable,
}

impl CredentialProvider for Provider {
    fn credential(&self) -> impl Future<Output = Result<LiveCredential, CallError>> + Send {
        std::future::ready(self.acquire())
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
            Self::Unavailable => Err(CallError::Authorization),
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
    let mut config = None;
    let mut trace = None;
    let mut health = None;
    let (pairs, remainder) = args.as_chunks::<2>();
    for pair in pairs {
        let target = match pair[0].as_str() {
            "--config" => &mut config,
            "--trace" => &mut trace,
            "--health-listen" => &mut health,
            _ => return Err(Error::Input),
        };
        if target.replace(pair[1].clone()).is_some() {
            return Err(Error::Input);
        }
    }
    if !remainder.is_empty() {
        return Err(Error::Input);
    }
    if lab.iter().any(Option::is_some) {
        if config.is_some() || trace.is_some() {
            return Err(Error::Input);
        }
        let [Some(wyoming), Some(url), Some(path)] = lab else {
            return Err(Error::Input);
        };
        return Ok(Some(Settings {
            wyoming: loopback(&wyoming, false)?,
            live_url: lab_origin(&url)?,
            trace: path.into(),
            health: loopback(health.as_deref().unwrap_or("127.0.0.1:0"), true)?,
            mode: LiveAuthMode::Chatgpt,
            voice: "alloy".into(),
            credential: Provider::Synthetic,
            lab: true,
            reply_lead_ms: 200,
            session_idle_seconds: 120,
        }));
    }
    configured(
        config.ok_or(Error::Input)?,
        trace.ok_or(Error::Input)?,
        health.as_deref(),
    )
}

fn configured(config: String, trace: String, health: Option<&str>) -> Result<Option<Settings>, Error> {
    let config = echo_config::BridgeConfig::load(config).map_err(|_| Error::Input)?;
    let mode = match config.live.mode {
        echo_config::LiveMode::Chatgpt => LiveAuthMode::Chatgpt,
        echo_config::LiveMode::ApiKey => LiveAuthMode::ApiKey,
    };
    let (live_url, credential) = match mode {
        LiveAuthMode::Chatgpt => ("https://chatgpt.com", Provider::Unavailable),
        LiveAuthMode::ApiKey => ("https://api.openai.com", Provider::ApiKey(config.live.credential_file)),
    };
    Ok(Some(Settings {
        wyoming: loopback(&format!("{}:{}", config.echo.host, config.echo.wyoming_port), false)?,
        live_url: live_url.into(),
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
