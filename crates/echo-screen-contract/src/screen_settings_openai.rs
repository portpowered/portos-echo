use super::{PortosChallengeV2, PortosOperationStateV2, SettingsVersionV2, present};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpenAiLinkStatusV2 {
    Checking,
    Unavailable,
    LinkRequired,
    Linked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenAiAccountV2 {
    pub version: SettingsVersionV2,
    pub link_status: OpenAiLinkStatusV2,
    pub operation_state: PortosOperationStateV2,
    #[schemars(with = "PortosChallengeV2")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub challenge: Option<PortosChallengeV2>,
    /// Present only when `live.mode = "api_key"`; never the key or any part of it.
    #[schemars(with = "OpenAiApiKeyStatusV2")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub api_key: Option<OpenAiApiKeyStatusV2>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenAiApiKeyStatusV2 {
    /// A well-formed key file exists at `live.credential_file`.
    pub configured: bool,
}

/// Private input, consumed explicitly at the bridge boundary. Neither Debug nor Display.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OpenAiApiKeyInput(String);

impl OpenAiApiKeyInput {
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

/// Replace the API key used in `api_key` mode. Write-only.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenAiApiKeyRequestV2 {
    pub version: SettingsVersionV2,
    #[schemars(with = "String", length(min = 1, max = 16384), extend("writeOnly" = true))]
    pub api_key: OpenAiApiKeyInput,
}
