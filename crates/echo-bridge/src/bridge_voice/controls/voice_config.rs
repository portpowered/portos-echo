//! Offline configuration admission, then explicitly nominated credential loading.
use super::{HostOptions, RuntimeOptions, ServeOptions, SessionOptions};
use crate::{Error, bridge_codex};
use echo_codex::DynamicTool;
use echo_config::{BridgeConfig, LiveMode};
use echo_contracts::secret::Secret;
use echo_link::LinkConnector;
use echo_media::MediaPeer;
use std::{future::Future, io::Read, net::SocketAddr, path::PathBuf, time::Duration};
use tokio::time::Instant;

/// Own paths whose borrows span the authenticated process and device lifetime.
/// Preparation never opens credentials, resolves a binary or creates a home.
pub struct Configured {
    config: BridgeConfig,
    kill_path: PathBuf,
    trace: PathBuf,
    health: SocketAddr,
    routes: Option<crate::bridge_operator::LocalRoutes>,
    utc_day: std::sync::Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl Configured {
    /// Admit the Codex backend without acquiring any runtime resource.
    /// # Errors
    /// Missing home or a backend outside this voice owner's responsibility.
    pub fn new(config: BridgeConfig, trace: PathBuf, health: SocketAddr) -> Result<Self, Error> {
        if config.codex.home.is_none() {
            return Err(Error::CodexHomeRequired);
        }
        if config.agent_backend != echo_config::AgentBackend::Client {
            return Err(Error::Input);
        }
        Ok(Self {
            kill_path: config.audit.log_dir.join("bridge-kill"),
            config,
            trace,
            health,
            routes: None,
            utc_day: std::sync::Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_secs() / 86_400)
            }),
        })
    }

    /// Route only the owned child to validated local fakes; no global mutation.
    #[must_use]
    pub fn with_routes(mut self, routes: crate::bridge_operator::LocalRoutes) -> Self {
        self.routes = Some(routes);
        self
    }

    #[must_use]
    pub fn with_clock(mut self, utc_day: std::sync::Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        self.utc_day = utc_day;
        self
    }

    /// Run configured voice ownership with caller-owned tools and media factory.
    /// The executable's connector/factory and controlled component ports share
    /// this boundary. Authentication always precedes device admission.
    /// # Errors
    /// Fixed credential, process, call, device and cleanup categories only.
    pub async fn execute<C: LinkConnector, P: MediaPeer>(
        self,
        connector: C,
        peer: impl AsyncFnMut() -> Result<P, Error>,
        tools: Vec<DynamicTool>,
        cancelled: impl Future<Output = ()>,
    ) -> Result<(), Error> {
        let api_key = self.api_key()?;
        let options = self.options(api_key.as_ref(), tools);
        super::execute(options, connector, peer, cancelled).await
    }

    fn api_key(&self) -> Result<Option<Secret<String>>, Error> {
        if self.config.live.mode == LiveMode::Chatgpt {
            return Ok(None);
        }
        let file = std::fs::File::open(&self.config.live.credential_file).map_err(|_| Error::Authorization)?;
        let mut bytes = Vec::new();
        file.take(16_385)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Authorization)?;
        if bytes.len() > 16_384 {
            return Err(Error::Authorization);
        }
        let text = String::from_utf8(bytes).map_err(|_| Error::Authorization)?;
        let key = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .unwrap_or(&text);
        if key.trim().is_empty() || key.chars().any(char::is_control) {
            return Err(Error::Authorization);
        }
        Ok(Some(Secret::new(key.into())))
    }

    fn options<'a>(&'a self, api_key: Option<&'a Secret<String>>, tools: Vec<DynamicTool>) -> RuntimeOptions<'a> {
        RuntimeOptions {
            host: HostOptions {
                config: &self.config.codex,
                mode: self.config.live.mode,
                routes: self.routes.as_ref(),
                api_key,
                deadline: Instant::now() + Duration::from_secs(30),
            },
            voice: ServeOptions {
                kill_path: Some(&self.kill_path),
                call: bridge_codex::Options {
                    voice: self.config.codex_voice(),
                    instructions: &self.config.codex.instructions,
                    model: self.config.codex_model(),
                },
                tools,
                session: SessionOptions {
                    idle: Duration::from_secs(self.config.live.session_idle_seconds),
                    response_timeout: Duration::from_secs(self.config.agent.task_deadline_seconds),
                    played_timeout: Duration::from_secs(5),
                    reply_lead_ms: self.config.audio.reply_lead_ms,
                    muted: false,
                },
                connect_timeout: Duration::from_secs(15),
            },
            screen: Some(&self.config.screen),
            health: self.health,
            trace: &self.trace,
            delegation: Some(&self.config),
            utc_day: self.utc_day.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured() -> Configured {
        let mut config =
            BridgeConfig::from_toml_str(include_str!("../../../../../examples/bridge.example.toml")).unwrap();
        config.codex.home = Some(std::env::temp_dir().join("unused-codex-config-home"));
        Configured::new(config, "unused-trace".into(), ([127, 0, 0, 1], 0).into()).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn configured_options_preserve_overrides_budgets_tools_and_default_voice() {
        let mut prepared = configured();
        prepared.config.codex.voice = Some("cedar".into());
        prepared.config.live.voice = Some("alloy".into());
        prepared.config.codex.model = Some("configured-model".into());
        prepared.config.agent.model = Some("legacy-model".into());
        prepared.config.codex.instructions = "configured instructions".into();
        prepared.config.live.session_idle_seconds = 43;
        prepared.config.agent.task_deadline_seconds = 17;
        prepared.config.audio.reply_lead_ms = 120;
        let tool = DynamicTool {
            name: "test.echo".into(),
            description: "Echo".into(),
            input_schema: serde_json::json!({"type":"object"}),
        };
        let options = prepared.options(None, vec![tool.clone()]);
        assert_eq!(options.voice.call.voice, Some("cedar"));
        assert_eq!(options.voice.call.model, Some("configured-model"));
        assert_eq!(options.voice.call.instructions, "configured instructions");
        assert_eq!(options.voice.tools, [tool]);
        assert_eq!(options.voice.session.idle, Duration::from_secs(43));
        assert_eq!(options.voice.session.response_timeout, Duration::from_secs(17));
        assert_eq!(options.voice.session.reply_lead_ms, 120);
        assert_eq!(options.voice.kill_path, Some(prepared.kill_path.as_path()));
        assert_eq!(options.host.deadline, Instant::now() + Duration::from_secs(30));
        prepared.config.codex.voice = None;
        prepared.config.codex.model = None;
        assert_eq!(prepared.options(None, vec![]).voice.call.voice, Some("alloy"));
        assert_eq!(prepared.options(None, vec![]).voice.call.model, Some("legacy-model"));
        prepared.config.live.voice = None;
        prepared.config.agent.model = None;
        assert_eq!(prepared.options(None, vec![]).voice.call.voice, None);
        assert_eq!(prepared.options(None, vec![]).voice.call.model, None);
    }

    #[test]
    fn preparation_and_chatgpt_never_open_legacy_credentials_or_home() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = configured().config;
        config.codex.home = None;
        assert!(matches!(
            Configured::new(config.clone(), "unused".into(), ([127, 0, 0, 1], 0).into()),
            Err(Error::CodexHomeRequired)
        ));
        config.codex.home = Some(directory.path().join("uncreated-home"));
        config.live.mode = LiveMode::Chatgpt;
        config.live.credential_file = directory.path().join("unreadable-credential");
        std::fs::create_dir(&config.live.credential_file).unwrap();
        let prepared = Configured::new(
            config,
            directory.path().join("uncreated-trace"),
            ([127, 0, 0, 1], 0).into(),
        )
        .unwrap();
        assert!(prepared.api_key().unwrap().is_none());
        assert!(!directory.path().join("uncreated-home").exists());
        assert!(!directory.path().join("uncreated-trace").exists());
    }

    #[test]
    fn api_key_reads_only_nominated_file_and_rejects_invalid_content() {
        let directory = tempfile::tempdir().unwrap();
        let mut prepared = configured();
        prepared.config.live.mode = LiveMode::ApiKey;
        prepared.config.live.credential_file = directory.path().join("key");
        assert!(matches!(prepared.api_key(), Err(Error::Authorization)));
        for invalid in [
            vec![],
            b" \n".to_vec(),
            b"synthetic\nextra".to_vec(),
            vec![255],
            vec![b'a'; 16_385],
        ] {
            std::fs::write(&prepared.config.live.credential_file, invalid).unwrap();
            assert!(matches!(prepared.api_key(), Err(Error::Authorization)));
        }
        for suffix in ["", "\n", "\r\n"] {
            std::fs::write(
                &prepared.config.live.credential_file,
                format!("synthetic-nominated-key{suffix}"),
            )
            .unwrap();
            assert_eq!(prepared.api_key().unwrap().unwrap().expose(), "synthetic-nominated-key");
        }
    }
}
