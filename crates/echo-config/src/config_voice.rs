//! Duplex run listening policy, separate from Live call reuse between wakes.
use crate::{ConfigError, config_decode::Fields};

/// Waiting after activity and the total limit for agent-granted extensions.
/// Active speech and playback are never interrupted by the extension limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceConfig {
    pub session_listen_enabled: bool,
    pub session_listen_seconds: u32,
    pub session_max_seconds: u32,
    /// How long a user speech start with no transcript text counts as speech.
    pub speech_start_expiry_seconds: u32,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            session_listen_enabled: true,
            session_listen_seconds: 15,
            session_max_seconds: 600,
            speech_start_expiry_seconds: 20,
        }
    }
}

impl VoiceConfig {
    pub(super) fn validate(self) -> Result<(), ConfigError> {
        if !(3..=300).contains(&self.session_listen_seconds) {
            return Err(ConfigError::new(
                "voice.session_listen_seconds",
                "must be within 3..=300",
            ));
        }
        if !(3..=2_147_483_647).contains(&self.session_max_seconds) {
            return Err(ConfigError::new(
                "voice.session_max_seconds",
                "must be within 3..=2147483647",
            ));
        }
        if !(3..=300).contains(&self.speech_start_expiry_seconds) {
            return Err(ConfigError::new(
                "voice.speech_start_expiry_seconds",
                "must be within 3..=300",
            ));
        }
        if self.session_max_seconds < self.session_listen_seconds {
            return Err(ConfigError::new(
                "voice.session_max_seconds",
                "must be at least voice.session_listen_seconds",
            ));
        }
        Ok(())
    }
}

pub(super) fn decode(value: toml::de::DeValue<'_>) -> Result<VoiceConfig, ConfigError> {
    let mut f = Fields::new(value, "voice")?;
    let defaults = VoiceConfig::default();
    let result = VoiceConfig {
        session_listen_enabled: f
            .optional("session_listen_enabled")?
            .unwrap_or(defaults.session_listen_enabled),
        session_listen_seconds: f
            .optional("session_listen_seconds")?
            .unwrap_or(defaults.session_listen_seconds),
        session_max_seconds: f
            .optional("session_max_seconds")?
            .unwrap_or(defaults.session_max_seconds),
        speech_start_expiry_seconds: f
            .optional("speech_start_expiry_seconds")?
            .unwrap_or(defaults.speech_start_expiry_seconds),
    };
    f.finish()?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use crate::BridgeConfig;

    const MINIMAL: &str = "[echo]\nhost='echo.example.invalid'\n[live]\ncredential_file='absent/live'\n[agent]\n[mcp]\n[screen]\npairing_token_file='absent/pairing'\n[audit]\nlog_dir='absent/logs'\n";

    #[test]
    fn voice_listen_rejects_invalid_ranges_and_nonintegers_without_values() {
        for field in [
            "session_listen_seconds",
            "session_max_seconds",
            "speech_start_expiry_seconds",
        ] {
            let short = field != "session_max_seconds";
            let range = if short { "3..=300" } else { "3..=2147483647" };
            let overflow = if short { "301" } else { "2147483648" };
            for value in ["0", "2", overflow, "4294967295"] {
                let error = BridgeConfig::from_toml_str(&format!("{MINIMAL}\n[voice]\n{field}={value}\n")).unwrap_err();
                assert_eq!(error.to_string(), format!("voice.{field}: must be within {range}"));
            }
            for value in [
                "-1",
                "4294967296",
                "9223372036854775807",
                "true",
                "15.0",
                "'SYNTHETIC_VALUE'",
                "[]",
                "{}",
            ] {
                let error = BridgeConfig::from_toml_str(&format!("{MINIMAL}\n[voice]\n{field}={value}\n")).unwrap_err();
                assert_eq!(error.to_string(), format!("voice.{field}: invalid type or value"));
                assert!(!format!("{error:?}").contains("SYNTHETIC_VALUE"));
            }
        }
    }

    #[test]
    fn voice_table_unknown_fields_and_nested_secrets_use_trusted_paths() {
        for (fields, expected) in [
            ("[voice]\nSYNTHETIC_VALUE=3", "voice: unknown field"),
            (
                "[voice]\nSYNTHETIC_VALUE={secret='SYNTHETIC_VALUE'}",
                "voice.secret: secrets go in files, not config",
            ),
            (
                "[voice]\nsession_listen_seconds={secret='SYNTHETIC_VALUE'}",
                "voice.session_listen_seconds.secret: secrets go in files, not config",
            ),
            (
                "[voice]\npassword='SYNTHETIC_VALUE'",
                "voice.password: secrets go in files, not config",
            ),
        ] {
            let error = BridgeConfig::from_toml_str(&format!("{MINIMAL}\n{fields}\n")).unwrap_err();
            assert_eq!(error.to_string(), expected);
            assert!(!format!("{error:?}").contains("SYNTHETIC_VALUE"));
        }
        for value in ["true", "15", "'SYNTHETIC_VALUE'", "[]"] {
            let error = BridgeConfig::from_toml_str(&format!("voice={value}\n{MINIMAL}")).unwrap_err();
            assert_eq!(error.to_string(), "voice: expected table");
        }
    }
}
