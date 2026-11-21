use crate::{BridgeConfig, ConfigError, config_decode::Fields};
use std::path::PathBuf;
use toml::de::DeValue;

/// How `binary` is asked to serve: the pinned multi-tool takes the `app-server`
/// subcommand; a standalone app-server build (the armv7 one on the Echo) takes none.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexLaunch {
    #[default]
    Subcommand,
    Standalone,
}

/// Hosted read-only search modes admitted by the bridge at the pinned server.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSearch {
    #[default]
    Live,
    Cached,
    Disabled,
}

impl WebSearch {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Cached => "cached",
            Self::Disabled => "disabled",
        }
    }
}

/// Offline settings for the app server. Runtime admission requires `home`;
/// parsing never resolves the binary, expands paths or reads credentials.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CodexConfig {
    pub home: Option<PathBuf>,
    pub binary: Option<PathBuf>,
    pub launch: CodexLaunch,
    /// A PEM bundle the child receives as `SSL_CERT_FILE`; opened only by the child.
    pub ca_file: Option<PathBuf>,
    pub voice: Option<String>,
    pub model: Option<String>,
    pub delegate_model: Option<String>,
    pub delegate_effort: Option<String>,
    pub delegate_fast: bool,
    pub time_zone: Option<String>,
    pub web_search: WebSearch,
    pub skills: Vec<String>,
}

/// Owner, 2026-11-18: conversation is the default and a hand-off is the exception.
/// Device, 2026-11-21: "answer directly" and "if unsure, answer instead" kept the
/// Live voice from delegating a hold or an end right after it answered a question.
pub const LIVE_INSTRUCTIONS: &str = r#"You are a friendly voice assistant on a smart speaker. Answer briefly.

Waiting and ending are background-agent tasks. When the user asks you to wait, hold on, pause, give them a minute, or end, stop or hang up the call, delegate it to the background agent with the user's exact words before you say anything. Do this every time, also right after you answered another question. Never handle these yourself: your words alone cannot keep the call open or close it. Saying goodbye does not end the call; only the delegation does, so delegate first and say goodbye only after it.
User: What time is it? You: It's ten past three. User: Hold on for two minutes. You: [delegate "Hold on for two minutes."] Sure, I'll wait.
User: What's twelve times four? You: Forty-eight. User: End the call now. You: [delegate "End the call now."] Okay, ending the call. Goodbye!
User: Thanks, that's all. You: [delegate "Thanks, that's all."] Bye for now!

Talk with the user yourself for small talk, opinions, explanations, general knowledge and arithmetic. Delegate actions on devices or services, and live or personal data you do not have. After a delegation, say one short acknowledgement and wait for its result; never delegate the same request twice. The context is a call-start snapshot: answer time, date, and day-of-week questions yourself from it. When no operator time zone is configured, UTC does not establish the user's local time zone."#;

/// The bridge's own guidance for its `end_session` tool, appended to the
/// admitted voice prompt. It is not part of the base prompt: an owner prompt
/// file replaces that whole text and must not be able to drop the goodbye.
pub const LIVE_SESSION_GUIDANCE: &str = r#"Waiting and ending the call are background-agent tasks: only the background agent can keep the call open or close it, and your own words do neither. When the user asks you to wait or hold on, such as hold on for two minutes, give me a minute, one moment, or I'll be right back, or to end the call, such as goodbye, that's all, thanks that's it, end the call, hang up, stop, never mind, or go to sleep, first delegate it to the background agent with the user's exact words. Do this every time, also right after you answered another question and even while another task is pending: ending the call supersedes it. Speak only after the delegation: a brief acknowledgement, or a brief goodbye for an end. If the user thanks you and also asks a question, answer the question and keep the call open.
User: How far away is the Moon? You: About 384,000 kilometres. User: Hold on for two minutes. You: [delegate "Hold on for two minutes."] Sure, I'll wait.
User: What's twelve times four? You: Forty-eight. User: End the call now. You: [delegate "End the call now."] Okay, ending the call. Goodbye!
User: Thanks, that's all. You: [delegate "Thanks, that's all."] Bye for now!"#;

/// The runner's half of the same guidance, appended to its admitted prompt.
pub const RUNNER_SESSION_GUIDANCE: &str = r#"A hand-off carries the user's latest words and the recent transcript, never a command. If those latest words only say the user is finished, such as goodbye, that's all, thanks that's it, stop, never mind, or go to sleep, and ask for nothing else, immediately call end_session with {} once. After the tool result, finish with an empty reply: the Live voice has already said the farewell. If the user also asks for something, such as "thanks, and what time is it?", do that and do not end the session. If those latest words ask you to wait or hold on for a while, call extend_session once with that many seconds, such as {"seconds":120} for two minutes, then finish with an empty reply."#;

/// Built-in delegated runner instructions for homes without a prompt file.
pub const RUNNER_INSTRUCTIONS: &str = r"Client audio bridge

For Port OS device control, resolve the named room or device with group_query or endpoint_query. If no target matches, explain that. If several match, ask which one. Copy the discovered ID and message into control and call it once per resolved action with an idempotencyKey. Lights need no extra confirmation. Describe a queued or pending action as submitted and stop. Even a successful delivery receipt only confirms delivery. When checking the effect of a completed light command, read the resolved lights' fresh power state once. Report only the confirmed result or uncertainty in one short sentence. If the tool says to relink Port OS, relay that instruction.";

/// The names Settings offers before the running app server has listed its own.
/// No Codex release is pinned (owner, 2026-11-21), so the app server is the
/// authority: the bridge reads `thread/realtime/listVoices` for the realtime
/// version it sends (`"v3"`), offers that list, and leaves an unlisted voice
/// out of the start. These nine have been v3 voices in every release since
/// the floor in `codex.release`.
/// `default` is ours: it omits the field so the app server picks its voice.
const REALTIME_VOICES: [&str; 9] = [
    "arbor", "breeze", "cove", "ember", "juniper", "maple", "sol", "spruce", "vale",
];

/// The v2-only voices no v3 list has carried. Settings offered them before the
/// bridge sent v3, so a stored one still loads, as `default` with a warning;
/// it is never sent and cannot be saved.
const RETIRED_VOICES: [&str; 9] = [
    "alloy", "ash", "ballad", "cedar", "coral", "echo", "sage", "shimmer", "verse",
];

pub(crate) const RETIRED_VOICE: &str = "not supported by the v3 realtime version; using default";

/// The built-in choices; `default` omits the outgoing voice field.
pub fn realtime_voices() -> impl Iterator<Item = &'static str> {
    std::iter::once("default").chain(REALTIME_VOICES)
}

pub(crate) fn retired_voice(voice: &str) -> bool {
    RETIRED_VOICES.contains(&voice)
}

/// Replace a retired startup voice with `default`, so no later sparse merge
/// can send it. The legacy Live path keeps `live.voice`, and stays quiet.
pub(crate) fn retire_base_voice(config: &mut BridgeConfig) -> Option<ConfigError> {
    if !config.codex_voice().is_some_and(retired_voice) {
        return None;
    }
    let field = if config.codex.voice.is_some() {
        "codex.voice"
    } else {
        "live.voice"
    };
    config.codex.voice = Some("default".into());
    (field == "codex.voice" || config.codex.home.is_some())
        .then(|| ConfigError::new("bridge.toml", format!("ignored {field}: {RETIRED_VOICE}")))
}

impl BridgeConfig {
    /// Omit the voice when neither setting is supplied, or the chosen one is
    /// `default`, using the app server's default.
    #[must_use]
    pub fn codex_voice(&self) -> Option<&str> {
        self.codex
            .voice
            .as_deref()
            .or(self.live.voice.as_deref())
            .filter(|voice| *voice != "default")
    }

    /// The delegated thread's model; the realtime model stays owned by the app server.
    #[must_use]
    pub fn codex_model(&self) -> Option<&str> {
        self.codex
            .delegate_model
            .as_deref()
            .or(self.codex.model.as_deref())
            .or(self.agent.model.as_deref())
    }
}

pub(super) fn decode(value: DeValue<'_>) -> Result<CodexConfig, ConfigError> {
    let mut fields = Fields::new(value, "codex")?;
    fields.reject("instructions", "removed; use <home>/prompts/runner.md")?;
    fields.reject("live_instructions", "removed; use <home>/prompts/voice.md")?;
    let config = CodexConfig {
        home: fields.optional("home")?,
        binary: fields.optional("binary")?,
        launch: fields.optional("launch")?.unwrap_or_default(),
        ca_file: fields.optional("ca_file")?,
        voice: fields.optional("voice")?,
        model: fields.optional("model")?,
        delegate_model: fields.optional("delegate_model")?,
        delegate_effort: fields.optional("delegate_effort")?,
        delegate_fast: fields.optional("delegate_fast")?.unwrap_or_default(),
        time_zone: fields.optional("time_zone")?,
        web_search: fields.optional("web_search")?.unwrap_or_default(),
        skills: fields.optional("skills")?.unwrap_or_default(),
    };
    fields.finish()?;
    Ok(config)
}

impl BridgeConfig {
    /// Validate the selection before the runtime uses any name as a path component.
    /// Parsing does not inspect which skills are installed.
    ///
    /// # Errors
    /// Requires the configured client runner and agent home for a nonempty selection.
    /// Names must be distinct, single path components.
    pub fn validate_skill_selection(&self) -> Result<(), ConfigError> {
        if self.codex.skills.is_empty() {
            return Ok(());
        }
        if self.codex.home.is_none() || self.home.is_none() {
            return Err(ConfigError::new(
                "codex.skills",
                "requires the Codex client runner and home.dir",
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for (index, name) in self.codex.skills.iter().enumerate() {
            let reason = if name.trim().is_empty()
                || name == "."
                || name.contains("..")
                || name.contains(['/', '\\', '\u{2028}', '\u{2029}'])
                || name.chars().any(char::is_control)
            {
                "expected a single skill name"
            } else if !seen.insert(name) {
                "duplicate skill name"
            } else {
                continue;
            };
            return Err(ConfigError::new(format!("codex.skills[{index}]"), reason));
        }
        Ok(())
    }
}

/// Only the shape of a voice name is checked offline: which names exist is the
/// running app server's answer, not this crate's. The bridge leaves a voice the
/// server does not list out of the realtime start, and Settings refuses to
/// save one. A retired voice still loads; see `RETIRED_VOICES`.
pub(super) fn validate_voice(config: &BridgeConfig) -> Result<(), ConfigError> {
    for (field, voice) in [("codex.voice", &config.codex.voice), ("live.voice", &config.live.voice)] {
        if let Some(voice) = voice
            && !voice.trim().is_empty()
            && !(voice.len() <= 32 && voice.bytes().all(|byte| byte.is_ascii_lowercase()))
        {
            return Err(ConfigError::new(field, "expected default or a realtime voice name"));
        }
    }
    Ok(())
}

pub(super) fn validate(config: &CodexConfig) -> Result<(), ConfigError> {
    for (field, path) in [
        ("codex.home", &config.home),
        ("codex.binary", &config.binary),
        ("codex.ca_file", &config.ca_file),
    ] {
        if path.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err(ConfigError::new(field, "expected nonempty absolute path"));
        }
    }
    // The release helper resolves only the multi-tool, so a standalone build is always named.
    if config.launch == CodexLaunch::Standalone && config.binary.is_none() {
        return Err(ConfigError::new("codex.launch", "standalone requires codex.binary"));
    }
    for (field, value) in [
        ("codex.voice", config.voice.as_deref()),
        ("codex.model", config.model.as_deref()),
        ("codex.delegate_model", config.delegate_model.as_deref()),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            return Err(ConfigError::new(field, "must be nonblank"));
        }
    }
    if config.delegate_effort.as_ref().is_some_and(String::is_empty) {
        return Err(ConfigError::new("codex.delegate_effort", "must be nonempty"));
    }
    if let Some(zone) = &config.time_zone
        && !zone.split('/').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_+-".contains(&byte))
        })
    {
        return Err(ConfigError::new("codex.time_zone", "expected IANA-name syntax"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../../../examples/bridge.example.toml");

    #[test]
    fn agent_home_builtin_prompts_match_the_admitted_text() {
        assert_eq!(
            LIVE_INSTRUCTIONS,
            r#"You are a friendly voice assistant on a smart speaker. Answer briefly.

Waiting and ending are background-agent tasks. When the user asks you to wait, hold on, pause, give them a minute, or end, stop or hang up the call, delegate it to the background agent with the user's exact words before you say anything. Do this every time, also right after you answered another question. Never handle these yourself: your words alone cannot keep the call open or close it. Saying goodbye does not end the call; only the delegation does, so delegate first and say goodbye only after it.
User: What time is it? You: It's ten past three. User: Hold on for two minutes. You: [delegate "Hold on for two minutes."] Sure, I'll wait.
User: What's twelve times four? You: Forty-eight. User: End the call now. You: [delegate "End the call now."] Okay, ending the call. Goodbye!
User: Thanks, that's all. You: [delegate "Thanks, that's all."] Bye for now!

Talk with the user yourself for small talk, opinions, explanations, general knowledge and arithmetic. Delegate actions on devices or services, and live or personal data you do not have. After a delegation, say one short acknowledgement and wait for its result; never delegate the same request twice. The context is a call-start snapshot: answer time, date, and day-of-week questions yourself from it. When no operator time zone is configured, UTC does not establish the user's local time zone."#
        );
        assert_eq!(
            RUNNER_INSTRUCTIONS,
            r"Client audio bridge

For Port OS device control, resolve the named room or device with group_query or endpoint_query. If no target matches, explain that. If several match, ask which one. Copy the discovered ID and message into control and call it once per resolved action with an idempotencyKey. Lights need no extra confirmation. Describe a queued or pending action as submitted and stop. Even a successful delivery receipt only confirms delivery. When checking the effect of a completed light command, read the resolved lights' fresh power state once. Report only the confirmed result or uncertainty in one short sentence. If the tool says to relink Port OS, relay that instruction."
        );
        // The goodbye guidance is the bridge's own, never part of a base prompt.
        for base in [LIVE_INSTRUCTIONS, RUNNER_INSTRUCTIONS] {
            assert!(!base.contains("end_session") && !base.contains("farewell"));
        }
        assert!(RUNNER_SESSION_GUIDANCE.contains("immediately call end_session with {} once"));
        assert!(RUNNER_SESSION_GUIDANCE.contains("finish with an empty reply"));
        // The base prompt no longer argues against a hand-off.
        assert!(
            !LIVE_INSTRUCTIONS.contains("answer directly") && !LIVE_INSTRUCTIONS.contains("instead of handing off")
        );
        assert!(
            RUNNER_SESSION_GUIDANCE
                .contains(r#"call extend_session once with that many seconds, such as {"seconds":120}"#)
        );
    }

    #[test]
    fn live_session_guidance_delegates_waiting_and_ending_first() {
        // Device, 2026-11-21: after an answered question the Live voice spoke an
        // acknowledgement or a farewell and never delegated, so a hold or an end
        // did nothing. Waiting and ending are tasks it delegates first, every time.
        assert!(LIVE_SESSION_GUIDANCE.starts_with("Waiting and ending the call are background-agent tasks"));
        assert!(LIVE_SESSION_GUIDANCE.contains("hold on for two minutes"));
        assert!(
            LIVE_SESSION_GUIDANCE.contains("first delegate it to the background agent with the user's exact words")
        );
        assert!(LIVE_SESSION_GUIDANCE.contains("also right after you answered another question"));
        // Nothing is spoken before the delegation: no farewell instruction leads it.
        let delegate = LIVE_SESSION_GUIDANCE.find("first delegate").unwrap();
        assert!(LIVE_SESSION_GUIDANCE.find("Speak only after the delegation").unwrap() > delegate);
        assert!(!LIVE_SESSION_GUIDANCE.contains("farewell"));
        for example in LIVE_SESSION_GUIDANCE.lines().skip(1) {
            let (before, after) = example.split_once("You: [delegate").unwrap();
            assert!(!before.to_lowercase().contains("bye") && after.contains(']'));
        }
    }

    #[test]
    fn web_search_modes_default_live_round_trip_and_reject_input_safely() {
        let load = |fields: &str| BridgeConfig::from_toml_str(&format!("{EXAMPLE}\n[codex]\n{fields}"));
        assert_eq!(
            BridgeConfig::from_toml_str(EXAMPLE).unwrap().codex.web_search,
            WebSearch::Live
        );
        assert_eq!(load("").unwrap().codex.web_search, WebSearch::Live);
        for mode in [WebSearch::Live, WebSearch::Cached, WebSearch::Disabled] {
            let encoded = toml::to_string(&std::collections::BTreeMap::from([("web_search", mode)])).unwrap();
            assert_eq!(load(&encoded).unwrap().codex.web_search, mode);
            assert_eq!(
                load(&format!("web_search = '{}'", mode.as_str()))
                    .unwrap()
                    .codex
                    .web_search,
                mode
            );
        }
        for value in ["'unknown'", "''", "'Live'", "'indexed'", "true", "7", "[]", "{}"] {
            assert_eq!(
                load(&format!("web_search = {value}")).unwrap_err().to_string(),
                "codex.web_search: invalid type or value"
            );
        }
        assert_eq!(
            BridgeConfig::from_toml_str(include_str!("../../../examples/bridge.echo.toml"))
                .unwrap()
                .codex
                .web_search,
            WebSearch::Live
        );
    }

    #[test]
    fn launch_form_and_ca_file_default_off_and_are_validated_offline() {
        let load = |codex: &str| BridgeConfig::from_toml_str(&format!("{EXAMPLE}\n[codex]\n{codex}\n"));
        let unset = BridgeConfig::from_toml_str(EXAMPLE).unwrap().codex;
        assert_eq!((unset.launch, unset.ca_file), (CodexLaunch::Subcommand, None));
        assert_eq!(
            load("launch = 'subcommand'").unwrap().codex.launch,
            CodexLaunch::Subcommand
        );
        // Neither path exists: parsing stays offline and never opens them.
        let device = load("binary = '/absent/app-server'\nlaunch = 'standalone'\nca_file = '/absent/ca.pem'")
            .unwrap()
            .codex;
        assert_eq!(device.launch, CodexLaunch::Standalone);
        assert_eq!(device.ca_file, Some(PathBuf::from("/absent/ca.pem")));
        for (codex, expected) in [
            (
                "launch = 'standalone'",
                "codex.launch: standalone requires codex.binary",
            ),
            ("launch = 'wrapper'", "codex.launch: invalid type or value"),
            ("launch = true", "codex.launch: invalid type or value"),
            (
                "ca_file = 'relative/ca.pem'",
                "codex.ca_file: expected nonempty absolute path",
            ),
            ("ca_file = ''", "codex.ca_file: expected nonempty absolute path"),
            ("ca_file = 7", "codex.ca_file: invalid type or value"),
        ] {
            assert_eq!(load(codex).unwrap_err().to_string(), expected, "{codex}");
        }
    }

    #[test]
    fn echo_example_enables_portos_device_code_with_private_state() {
        let config = BridgeConfig::from_toml_str(include_str!("../../../examples/bridge.echo.toml")).unwrap();
        assert_eq!(config.mcp.operator_listen, "127.0.0.1:16902".parse().unwrap());
        assert_eq!(config.mcp.server.len(), 1);
        let server = &config.mcp.server[0];
        assert_eq!(server.name, "portos");
        assert_eq!(server.url, "https://api.portoperatingsystem.com/mcp"); // lint-rules: allow-literal offline example assertion, never contacted
        assert_eq!(server.auth, crate::McpAuth::PortosDeviceCode);
        assert_eq!(server.bearer_file, None);
        assert_eq!(
            server.allow_tools,
            [
                "control",
                "endpoint_query",
                "endpoint_show",
                "endpoint_manage",
                "group_query",
                "group_manage",
                "guide",
                "subscription_list",
                "subscription_inspect",
                "subscription_cancel",
                "subscription_label_edit"
            ]
        );
        assert_eq!(
            config.mcp.scopes,
            [
                "endpoint:read",
                "endpoint:write",
                "message:send",
                "group:read",
                "group:manage",
                "subscription:read",
                "subscription:write"
            ]
        );
        assert_eq!(
            config.mcp.state_dir,
            Some(PathBuf::from("/data/local/echo-bridge/private/portos"))
        );
    }

    #[test]
    fn echo_example_is_a_private_standalone_install_with_loopback_listeners() {
        let config = BridgeConfig::from_toml_str(include_str!("../../../examples/bridge.echo.toml")).unwrap();
        assert_eq!(
            (config.echo.host.as_str(), config.echo.wyoming_port),
            ("127.0.0.1", 16700)
        );
        assert_eq!(config.screen.listen, "127.0.0.1:16900".parse().unwrap());
        assert_eq!(config.live.mode, crate::LiveMode::Chatgpt);
        assert_eq!(config.codex.launch, CodexLaunch::Standalone);
        let private = std::path::Path::new("/data/local/echo-bridge/private");
        for path in [
            config.codex.home.as_deref(),
            Some(config.live.credential_file.as_path()),
            Some(config.screen.pairing_token_file.as_path()),
            Some(config.audit.log_dir.as_path()),
        ] {
            assert_eq!(path.and_then(std::path::Path::parent), Some(private));
        }
        assert_eq!(
            config.codex.binary,
            Some(PathBuf::from("/data/local/echo-bridge/codex-app-server"))
        );
        assert_eq!(
            config.codex.ca_file,
            Some(PathBuf::from("/data/local/echo-bridge/cacert.pem"))
        );
    }

    #[test]
    fn agent_home_unchanged_builtin_runner_default() {
        assert!(RUNNER_INSTRUCTIONS.starts_with("Client audio bridge\n\nFor Port OS device control,"));
    }

    #[test]
    fn agent_home_removed_prompts_name_files_without_echoing_values() {
        for (key, file) in [("instructions", "runner"), ("live_instructions", "voice")] {
            for value in ["'discarded prompt'", "false", "''", "{ nested = 1 }"] {
                let input = format!("{EXAMPLE}\n[codex]\n{key} = {value}\n");
                assert_eq!(
                    BridgeConfig::from_toml_str(&input).unwrap_err().to_string(),
                    format!("codex.{key}: removed; use <home>/prompts/{file}.md")
                );
            }
        }
    }

    #[test]
    fn agent_home_is_optional_absolute_offline_and_sanitized() {
        assert_eq!(BridgeConfig::from_toml_str(EXAMPLE).unwrap().home, None);
        for (fields, expected) in [
            ("", "home.dir: missing required field"),
            ("dir=''", "home.dir: expected nonempty absolute path"),
            ("dir='relative'", "home.dir: expected nonempty absolute path"),
            ("dir='~/home'", "home.dir: expected nonempty absolute path"),
            ("dir=false", "home.dir: invalid type or value"),
            ("dir='/unused'\nunknown='discarded'", "home: unknown field"),
            (
                "dir='/unused'\npassword='discarded'",
                "home.password: secrets go in files, not config",
            ),
            (
                "dir='/unused'\nunknown={token='discarded'}",
                "home.token: secrets go in files, not config",
            ),
        ] {
            assert_eq!(
                BridgeConfig::from_toml_str(&format!("{EXAMPLE}\n[home]\n{fields}"))
                    .unwrap_err()
                    .to_string(),
                expected
            );
        }
    }
}
