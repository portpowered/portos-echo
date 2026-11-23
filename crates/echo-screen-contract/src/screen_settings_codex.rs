//! The Settings V2 OpenAI options: the hosted search mode and the delegated runner.
use super::{WebSearchV2, present};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The delegated runner's service tier. Ultrafast needs a model that lists it
/// (GPT-6 Astra or GPT-6.1 Sol); the app server serves an unlisted tier as standard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DelegateSpeedV2 {
    #[serde(rename = "standard")]
    Standard,
    #[serde(rename = "fast")]
    Fast,
    #[serde(rename = "ultrafast")]
    Ultrafast,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("minProperties" = 1))]
pub struct CodexPatchV2 {
    #[schemars(with = "String")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub voice: Option<String>,
    #[schemars(with = "WebSearchV2")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub web_search: Option<WebSearchV2>,
    #[schemars(with = "String")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub delegate_model: Option<String>,
    #[schemars(with = "String")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub delegate_effort: Option<String>,
    #[schemars(with = "DelegateSpeedV2")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub delegate_speed: Option<DelegateSpeedV2>,
    /// Deprecated compatibility input: true is `fast`, false is `standard`. Use `delegate_speed`.
    #[schemars(with = "bool")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub delegate_fast: Option<bool>,
}

impl CodexPatchV2 {
    /// The requested tier, from `delegate_speed` or the legacy `delegate_fast`.
    ///
    /// # Errors
    /// Both supplied and naming different tiers.
    pub fn requested_speed(&self) -> Result<Option<DelegateSpeedV2>, &'static str> {
        let legacy = self.delegate_fast.map(|fast| {
            if fast {
                DelegateSpeedV2::Fast
            } else {
                DelegateSpeedV2::Standard
            }
        });
        match (self.delegate_speed, legacy) {
            (Some(speed), Some(legacy)) if speed != legacy => {
                Err("codex.delegate_speed: conflicts with codex.delegate_fast")
            }
            (speed, legacy) => Ok(speed.or(legacy)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodexValuesV2 {
    pub voice: String,
    pub web_search: WebSearchV2,
    pub delegate_model: String,
    pub delegate_effort: String,
    pub delegate_speed: DelegateSpeedV2,
}
