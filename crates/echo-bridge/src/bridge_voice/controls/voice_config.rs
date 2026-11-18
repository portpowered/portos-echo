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
    mcp_epoch: Option<u64>,
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
            mcp_epoch: None,
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

    pub(crate) fn with_mcp_epoch(mut self, epoch: Option<u64>) -> Self {
        self.mcp_epoch = epoch;
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
            mcp_epoch: self.mcp_epoch,
        }
    }
}

/// Append a call-local snapshot. Std has no portable local-zone discovery.
pub(super) fn call_instructions(prefix: &str, now: std::time::SystemTime) -> Result<String, Error> {
    let seconds = match now.duration_since(std::time::UNIX_EPOCH) {
        Ok(value) => i64::try_from(value.as_secs()).map_err(|_| Error::Input)?,
        Err(value) => {
            let value = value.duration();
            -i64::try_from(value.as_secs()).map_err(|_| Error::Input)? - i64::from(value.subsec_nanos() != 0)
        }
    };
    if !(-62_167_219_200..=253_402_300_799).contains(&seconds) {
        return Err(Error::Input);
    }
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let (year, month, day) = calendar(days);
    let weekday = match (days + 4).rem_euclid(7) {
        0 => "Sunday",
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        _ => "Saturday",
    };
    Ok(format!(
        "{prefix}\n[Call-start context: {year:04}-{month:02}-{day:02} {weekday} {:02}:{:02}:{:02} UTC+00:00; local offset unavailable]",
        time / 3600,
        time / 60 % 60,
        time % 60
    ))
}

// March-based Gregorian eras: each 400-year era contains exactly 146097 days.
fn calendar(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let march_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * march_month + 2) / 5 + 1;
    let month = march_month + if march_month < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month, day)
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::{ServeNotice, ServeOptions, SessionNotice};
    use super::*;
    use echo_link::fake::Command;
    use echo_media::MediaPeer;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[test]
    fn context_fixed_utc_calendar_and_prefix() {
        for (epoch, expected) in [
            (0_i64, "1970-01-01 Thursday 00:00:00"), // lint-rules: allow-literal UTC calendar expectation, never an endpoint
            (-1, "1969-12-31 Wednesday 23:59:59"), // lint-rules: allow-literal UTC calendar expectation, never an endpoint
            (951_868_799, "2000-02-29 Tuesday 23:59:59"), // lint-rules: allow-literal UTC calendar expectation, never an endpoint
            (4_107_542_400, "2100-03-01 Monday 00:00:00"), // lint-rules: allow-literal UTC calendar expectation, never an endpoint
            (1_798_761_599, "2026-12-31 Thursday 23:59:59"), // lint-rules: allow-literal UTC calendar expectation, never an endpoint
            (1_798_761_600, "2027-01-01 Friday 00:00:00"), // lint-rules: allow-literal UTC calendar expectation, never an endpoint
            (1_794_918_896, "2026-11-17 Tuesday 12:34:56"), // lint-rules: allow-literal UTC calendar expectation, never an endpoint
        ] {
            let now = if epoch < 0 {
                std::time::UNIX_EPOCH - Duration::from_secs(epoch.unsigned_abs())
            } else {
                std::time::UNIX_EPOCH + Duration::from_secs(epoch.unsigned_abs())
            };
            for prefix in ["Client audio bridge", "\u{8a9e}\n[Call-start context: authored] \n"] {
                assert_eq!(
                    call_instructions(prefix, now).unwrap(),
                    format!("{prefix}\n[Call-start context: {expected} UTC+00:00; local offset unavailable]") // lint-rules: allow-literal UTC calendar expectation, never an endpoint
                );
            }
        }
        assert_eq!(
            call_instructions("", std::time::UNIX_EPOCH - Duration::from_nanos(100)).unwrap(),
            "\n[Call-start context: 1969-12-31 Wednesday 23:59:59 UTC+00:00; local offset unavailable]" // lint-rules: allow-literal UTC calendar expectation, never an endpoint
        );
        let invalid = std::time::UNIX_EPOCH + Duration::from_hours(70_389_528);
        assert_eq!(call_instructions("prefix", invalid), Err(Error::Input));
    }

    #[tokio::test(start_paused = true)]
    async fn configured_call_options_include_context_and_preserve_policy() {
        let prepared = configured().with_mcp_epoch(Some(123));
        assert_eq!(
            prepared.config.codex.instructions,
            "Client audio bridge. Answer directly when the conversation and call-start context are sufficient. Delegate only when a tool or external lookup is needed. The context is a call-start snapshot; UTC does not establish the user's local time zone."
        );
        let tool = DynamicTool {
            name: "test.echo".into(),
            description: "Echo".into(),
            input_schema: serde_json::json!({"type":"object"}),
        };
        let options = prepared.options(None, vec![tool.clone()]);
        let text = call_instructions(options.voice.call.instructions, std::time::UNIX_EPOCH).unwrap();
        let call = options.voice.conversation(&text, options.host.deadline);
        assert_eq!(
            call.call.instructions,
            format!(
                "{}\n[Call-start context: 1970-01-01 Thursday 00:00:00 UTC+00:00; local offset unavailable]", // lint-rules: allow-literal UTC calendar expectation, never an endpoint
                prepared.config.codex.instructions
            )
        );
        assert_eq!(call.call.voice, options.voice.call.voice);
        assert_eq!(call.call.model, options.voice.call.model);
        assert_eq!(call.tools, [tool]);
        assert_eq!(call.session.idle, options.voice.session.idle);
        assert_eq!(call.session.response_timeout, options.voice.session.response_timeout);
        assert_eq!(call.session.played_timeout, options.voice.session.played_timeout);
        assert_eq!(call.session.reply_lead_ms, options.voice.session.reply_lead_ms);
        assert_eq!(call.deadline, options.host.deadline);
        assert_eq!(options.voice.kill_path, Some(prepared.kill_path.as_path()));
        assert_eq!(options.mcp_epoch, Some(123));
        assert_eq!(options.voice.call.instructions, prepared.config.codex.instructions);
    }

    pub(in crate::bridge_voice) struct MemoryPeer;
    impl MediaPeer for MemoryPeer {
        fn create(_: echo_media::PeerConfig) -> impl Future<Output = Result<Self, echo_media::Error>> {
            std::future::ready(Ok(Self))
        }
        fn offer(&self) -> impl Future<Output = Result<String, echo_media::Error>> {
            std::future::ready(Ok("fixture-offer".into()))
        }
        fn apply_answer(&self, _: &str) -> impl Future<Output = Result<(), echo_media::Error>> {
            std::future::ready(Ok(()))
        }
        fn states(&self) -> tokio::sync::watch::Receiver<echo_media::PeerState> {
            tokio::sync::watch::channel(echo_media::PeerState::Connected).1
        }
        fn send(&self, _: echo_media::PcmFrame) -> Result<(), echo_media::Error> {
            Ok(())
        }
        async fn recv(&mut self) -> Result<Option<echo_media::PcmFrame>, echo_media::Error> {
            std::future::pending().await
        }
        fn dropped_frames(&self) -> echo_media::DropCounts {
            echo_media::DropCounts::default()
        }
        fn close(&mut self) -> impl Future<Output = Result<(), echo_media::Error>> {
            std::future::ready(Ok(()))
        }
    }
    pub(in crate::bridge_voice) fn admission_options() -> ServeOptions<'static> {
        ServeOptions {
            kill_path: None,
            call: bridge_codex::Options {
                voice: Some("cedar"),
                model: Some("fixture-model"),
                instructions: "configured\n\u{8a9e} \n",
            },
            tools: vec![],
            session: SessionOptions {
                idle: Duration::from_secs(1),
                response_timeout: Duration::from_secs(1),
                played_timeout: Duration::from_secs(1),
                reply_lead_ms: 200,
                muted: false,
            },
            connect_timeout: Duration::from_secs(2),
        }
    }
    pub(in crate::bridge_voice) async fn check_context(
        remote: (
            tokio::io::DuplexStream,
            &echo_link::fake::Observer,
            &tokio::sync::Notify,
        ),
        expected: String,
    ) {
        let (server, observer, started) = remote;
        let mut server = BufReader::new(server);
        for method in ["thread/start", "thread/realtime/start", "thread/realtime/stop"] {
            let mut line = String::new();
            server.read_line(&mut line).await.unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], method);
            let params = &request["params"];
            match method {
                "thread/start" => assert_eq!(params["baseInstructions"], expected),
                "thread/realtime/start" => {
                    assert_eq!(params["prompt"], expected);
                    assert_eq!(params["voice"], "cedar");
                    server.get_mut().write_all(b"{\"method\":\"thread/realtime/sdp\",\"params\":{\"threadId\":\"thread-1\",\"sdp\":\"fixture-answer\"}}\n").await.unwrap();
                }
                _ => {
                    server
                        .get_mut()
                        .write_all(b"{\"method\":\"thread/realtime/closed\",\"params\":{\"threadId\":\"thread-1\"}}\n")
                        .await
                        .unwrap();
                }
            }
            let response = serde_json::json!({"id": request["id"], "result": if method == "thread/start" { serde_json::json!({"thread":{"id":"thread-1"}}) } else { serde_json::json!({}) }});
            server
                .get_mut()
                .write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
            if method == "thread/realtime/start" {
                retained_replies(&mut server, observer, started).await;
            }
        }
    }
    pub(in crate::bridge_voice) fn retained_observer<'a>(
        observer: &'a echo_link::fake::Observer,
        started: &'a tokio::sync::Notify,
        completed: &'a mut usize,
    ) -> impl FnMut(ServeNotice) -> Result<(), Error> + 'a {
        move |notice| {
            match notice {
                ServeNotice::Session(SessionNotice::Started) => started.notify_one(),
                ServeNotice::Session(SessionNotice::Completed(_)) => {
                    *completed += 1;
                    if *completed == 1 {
                        observer.wake("retained", vec![]);
                    }
                }
                _ => (),
            }
            Ok(())
        }
    }
    async fn retained_replies(
        server: &mut BufReader<tokio::io::DuplexStream>,
        observer: &echo_link::fake::Observer,
        started: &tokio::sync::Notify,
    ) {
        for count in 1..=2 {
            started.notified().await;
            let done = serde_json::json!({"method":"thread/realtime/transcript/done",
                "params":{"threadId":"thread-1","role":"assistant","text":"fixture reply"}});
            server
                .get_mut()
                .write_all(format!("{done}\n").as_bytes())
                .await
                .unwrap();
            let stops = || observer.commands().iter().filter(|cmd| **cmd == Command::Stop).count();
            for _ in 0..1024 {
                if stops() == count {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert_eq!(stops(), count);
            observer.finish(Ok(()));
        }
    }
    fn configured() -> Configured {
        let mut config = BridgeConfig::from_toml_str(
            &include_str!("../../../../../examples/bridge.example.toml")
                .replace(
                    "# Optional Codex instructions (add a table named codex to customize):",
                    "[codex]",
                )
                .replace("# instructions =", "instructions ="),
        )
        .unwrap();
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
        assert_eq!(options.mcp_epoch, None);
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
        let prepared = prepared.with_mcp_epoch(Some(1_800_000_000));
        assert_eq!(prepared.options(None, vec![]).mcp_epoch, Some(1_800_000_000));
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
