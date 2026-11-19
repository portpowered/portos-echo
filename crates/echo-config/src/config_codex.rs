use crate::{BridgeConfig, ConfigError, config_decode::Fields};
use std::path::PathBuf;
use toml::de::DeValue;

/// Offline settings for the app server. Runtime admission requires `home`;
/// parsing never resolves the binary, expands paths or reads credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexConfig {
    pub home: Option<PathBuf>,
    pub binary: Option<PathBuf>,
    pub voice: Option<String>,
    /// The delegated turn's base instructions (`thread/start`), never the Live prompt.
    pub instructions: String,
    /// The GPT-Live prompt (`thread/realtime/start`): conversation first, hand-off the exception.
    pub live_instructions: String,
    pub model: Option<String>,
    pub delegate_model: Option<String>,
    pub delegate_effort: Option<String>,
    pub time_zone: Option<String>,
}

/// Owner, 2026-11-18: conversation is the default and a hand-off is the exception.
const LIVE_INSTRUCTIONS: &str = "You are a friendly voice assistant on a smart speaker. Conversation is the \
default: answer directly and briefly from the conversation, the call-start context and your own general knowledge. \
Greetings, small talk, opinions, explanations, general knowledge and arithmetic never need the background agent; do \
not hand off just to check or verify something you can answer. Hand off to the background agent only when the \
request truly needs a tool: an action on a device or service, or live or personal data you do not have. If you are \
unsure whether a tool is needed, answer or ask a short question instead of handing off. After a hand-off, say one \
short acknowledgement and wait for its result; never hand off the same request twice. The context is a call-start \
snapshot; UTC does not establish the user's local time zone.";

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            home: None,
            binary: None,
            voice: None,
            instructions: "Client audio bridge".into(),
            live_instructions: LIVE_INSTRUCTIONS.into(),
            model: None,
            delegate_model: None,
            delegate_effort: None,
            time_zone: None,
        }
    }
}

/// The app server's `RealtimeVoice` values at the pinned rust-v0.160.0 schema.
/// `default` is ours: it omits the field so the app server picks its voice.
const REALTIME_VOICES: [&str; 19] = [
    "alloy", "arbor", "ash", "ballad", "breeze", "cedar", "coral", "cove", "echo", "ember", "juniper", "maple",
    "marin", "sage", "shimmer", "sol", "spruce", "vale", "verse",
];

impl BridgeConfig {
    /// Omit the voice when neither setting is supplied, or the chosen one is
    /// `default`, using the pinned server default.
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
    let config = CodexConfig {
        home: fields.optional("home")?,
        binary: fields.optional("binary")?,
        voice: fields.optional("voice")?,
        instructions: fields
            .optional("instructions")?
            .unwrap_or_else(|| CodexConfig::default().instructions),
        live_instructions: fields
            .optional("live_instructions")?
            .unwrap_or_else(|| LIVE_INSTRUCTIONS.into()),
        model: fields.optional("model")?,
        delegate_model: fields.optional("delegate_model")?,
        delegate_effort: fields.optional("delegate_effort")?,
        time_zone: fields.optional("time_zone")?,
    };
    fields.finish()?;
    Ok(config)
}

/// An unknown voice is refused offline: the app server rejects the whole
/// realtime start as an invalid request, after the wake.
pub(super) fn validate_voice(config: &BridgeConfig) -> Result<(), ConfigError> {
    for (field, voice) in [("codex.voice", &config.codex.voice), ("live.voice", &config.live.voice)] {
        if let Some(voice) = voice
            && !voice.trim().is_empty()
            && voice != "default"
            && !REALTIME_VOICES.contains(&voice.as_str())
        {
            return Err(ConfigError::new(field, "expected default or a pinned realtime voice"));
        }
    }
    Ok(())
}

pub(super) fn validate(config: &CodexConfig) -> Result<(), ConfigError> {
    for (field, path) in [("codex.home", &config.home), ("codex.binary", &config.binary)] {
        if path.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err(ConfigError::new(field, "expected nonempty absolute path"));
        }
    }
    for (field, value) in [
        ("codex.voice", config.voice.as_deref()),
        ("codex.model", config.model.as_deref()),
        ("codex.delegate_model", config.delegate_model.as_deref()),
        ("codex.instructions", Some(config.instructions.as_str())),
        ("codex.live_instructions", Some(config.live_instructions.as_str())),
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
    fn live_prompt_defaults_to_conversation_and_never_shares_the_delegated_text() {
        let unset = BridgeConfig::from_toml_str(EXAMPLE).unwrap().codex;
        assert_eq!(unset.instructions, "Client audio bridge");
        assert_eq!(unset.live_instructions, LIVE_INSTRUCTIONS);
        assert!(LIVE_INSTRUCTIONS.contains("Conversation is the default: answer directly and briefly"));
        assert!(LIVE_INSTRUCTIONS.contains("do not hand off just to check or verify something you can answer"));
        assert!(
            LIVE_INSTRUCTIONS.contains("Hand off to the background agent only when the request truly needs a tool")
        );
        // The example documents the built-in prompt verbatim, so neither drifts.
        let documented = EXAMPLE
            .lines()
            .find_map(|line| line.strip_prefix("# live_instructions = \"\"\""))
            .and_then(|line| line.strip_suffix("\"\"\""));
        assert_eq!(documented, Some(LIVE_INSTRUCTIONS));
        // Each key is independent: neither text falls back to the other.
        for (key, delegated, live) in [
            ("instructions", "Only one", LIVE_INSTRUCTIONS),
            ("live_instructions", "Client audio bridge", "Only one"),
        ] {
            let codex = BridgeConfig::from_toml_str(&format!("{EXAMPLE}\n[codex]\n{key} = 'Only one'\n"))
                .unwrap()
                .codex;
            assert_eq!(
                (codex.instructions.as_str(), codex.live_instructions.as_str()),
                (delegated, live)
            );
        }
    }
}
