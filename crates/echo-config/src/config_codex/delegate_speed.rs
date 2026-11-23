//! The delegated runner's service tier, with the legacy `delegate_fast` boolean.
use crate::ConfigError;

/// How fast the delegated Codex thread is served. Faster tiers cost more and
/// need a model whose catalog lists them; the app server silently serves a
/// tier it cannot honour as standard.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegateSpeed {
    #[default]
    Standard,
    /// Priority processing.
    Fast,
    /// Needs a model that lists the tier, such as `gpt-6-astra` or `gpt-6.1-sol`.
    Ultrafast,
}

impl DelegateSpeed {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Fast => "fast",
            Self::Ultrafast => "ultrafast",
        }
    }

    /// The `thread/start` `serviceTier`; `None` is sent as a present null,
    /// which clears an inherited tier and selects standard routing.
    #[must_use]
    pub const fn service_tier(self) -> Option<&'static str> {
        match self {
            Self::Standard => None,
            Self::Fast => Some("priority"),
            Self::Ultrafast => Some("ultrafast"),
        }
    }

    /// The legacy boolean names exactly one tier: true is Fast, false Standard.
    #[must_use]
    pub const fn from_legacy_fast(fast: bool) -> Self {
        if fast { Self::Fast } else { Self::Standard }
    }

    /// Combine `delegate_speed` with the legacy `delegate_fast`, either optional.
    ///
    /// # Errors
    /// Both present and naming different tiers.
    pub fn resolve(speed: Option<Self>, legacy_fast: Option<bool>) -> Result<Option<Self>, ConfigError> {
        match (speed, legacy_fast.map(Self::from_legacy_fast)) {
            (Some(speed), Some(legacy)) if speed != legacy => Err(ConfigError::new(
                "codex.delegate_speed",
                "conflicts with codex.delegate_fast; remove delegate_fast",
            )),
            (speed, legacy) => Ok(speed.or(legacy)),
        }
    }
}
