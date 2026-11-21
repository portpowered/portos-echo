use super::{SettingsVersionV2, present};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Private inputs are consumed explicitly at the bridge boundary.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MusePrivateInput(String);

impl MusePrivateInput {
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

#[derive(Default, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("minProperties" = 1))]
pub struct MusePatchV2 {
    #[schemars(with = "String", url)]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub url: Option<String>,
    /// DER-encoded trust anchor.
    #[schemars(with = "String", extend("contentEncoding" = "base64", "writeOnly" = true))]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub ca: Option<MusePrivateInput>,
    #[schemars(with = "String", extend("writeOnly" = true))]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub credential: Option<MusePrivateInput>,
    #[schemars(with = "bool", extend("const" = true))]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub clear: Option<bool>,
}

impl MusePatchV2 {
    pub(super) fn valid_shape(&self) -> bool {
        let has_input = self.url.is_some() || self.ca.is_some() || self.credential.is_some();
        match self.clear {
            Some(clear) => clear && !has_input,
            None => {
                has_input
                    && self.url.as_ref().is_none_or(|value| !value.is_empty())
                    && self.ca.as_ref().is_none_or(|value| !value.0.is_empty())
                    && self.credential.as_ref().is_none_or(|value| !value.0.is_empty())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MuseReachabilityV2 {
    Unchecked,
    Reachable,
    Unreachable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MuseProbeReasonV2 {
    Unconfigured,
    InvalidConfiguration,
    Unauthorized,
    Unavailable,
    UnexpectedResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MuseStatusV2 {
    pub configured: bool,
    #[schemars(with = "String", url)]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub url: Option<String>,
    pub reachability: MuseReachabilityV2,
    #[schemars(with = "MuseProbeReasonV2")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub reason: Option<MuseProbeReasonV2>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum MuseProbeRequestV2 {
    Probe { version: SettingsVersionV2 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MuseProbeResultV2 {
    pub version: SettingsVersionV2,
    pub reachable: bool,
    #[schemars(with = "MuseProbeReasonV2")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub reason: Option<MuseProbeReasonV2>,
}
