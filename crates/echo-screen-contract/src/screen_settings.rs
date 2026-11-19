//! Strict HTTP settings messages; independent of legacy SSE/action messages.
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize};

/// The sole supported settings protocol version, encoded as the integer 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct SettingsVersionV1;

impl TryFrom<u8> for SettingsVersionV1 {
    type Error = &'static str;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value == 1 {
            Ok(Self)
        } else {
            Err("unsupported settings version")
        }
    }
}

impl From<SettingsVersionV1> for u8 {
    fn from(_: SettingsVersionV1) -> Self {
        1
    }
}

impl JsonSchema for SettingsVersionV1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SettingsVersionV1".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type": "integer", "const": 1})
    }
}

// An omitted patch field means unchanged. Explicit null is never an omission.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(decoder: D) -> Result<Option<T>, D::Error> {
    T::deserialize(decoder).map(Some)
}

/// Sparse editable leaves. Effective ranges/voices are validated by echo-config.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VoicePatchV1 {
    #[schemars(with = "bool")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub session_listen_enabled: Option<bool>,
    #[schemars(with = "u32")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub session_listen_seconds: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodexPatchV1 {
    pub voice: String,
}

/// Lossless decimal request cap. Semantic TOML upper bound belongs to echo-config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DailyCapV1(String);

impl TryFrom<String> for DailyCapV1 {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty()
            || value.len() > 19
            || !value.bytes().all(|byte| byte.is_ascii_digit())
            || (value.len() > 1 && value.starts_with('0'))
        {
            return Err("daily_request_cap must be canonical unsigned decimal text");
        }
        Ok(Self(value))
    }
}

impl From<DailyCapV1> for String {
    fn from(value: DailyCapV1) -> Self {
        value.0
    }
}

impl DailyCapV1 {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl JsonSchema for DailyCapV1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "DailyCapV1".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type": "string", "pattern": "^(0|[1-9][0-9]*)$", "maxLength": 19})
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentPatchV1 {
    #[schemars(with = "DailyCapV1")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub daily_request_cap: Option<DailyCapV1>,
    #[schemars(with = "u32")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub max_tool_calls: Option<u32>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsFieldsV1 {
    #[schemars(with = "VoicePatchV1")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub voice: Option<VoicePatchV1>,
    #[schemars(with = "CodexPatchV1")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub codex: Option<CodexPatchV1>,
    #[schemars(with = "AgentPatchV1")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub agent: Option<AgentPatchV1>,
}

impl SettingsFieldsV1 {
    /// Every supplied section must contain an editable leaf.
    #[must_use]
    pub fn is_nonempty(&self) -> bool {
        (self.voice.is_some() || self.codex.is_some() || self.agent.is_some())
            && self
                .voice
                .as_ref()
                .is_none_or(|v| v.session_listen_enabled.is_some() || v.session_listen_seconds.is_some())
            && self
                .agent
                .as_ref()
                .is_none_or(|v| v.daily_request_cap.is_some() || v.max_tool_calls.is_some())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatchV1 {
    pub version: SettingsVersionV1,
    pub settings: SettingsFieldsV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PromptSaveV1 {
    pub version: SettingsVersionV1,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VoiceValuesV1 {
    pub session_listen_enabled: bool,
    pub session_listen_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentValuesV1 {
    pub daily_request_cap: DailyCapV1,
    pub max_tool_calls: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsValuesV1 {
    pub voice: VoiceValuesV1,
    pub codex: CodexPatchV1,
    pub agent: AgentValuesV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PromptTextsV1 {
    pub voice: String,
    pub runner: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkStatusV1 {
    Unconfigured,
    LinkRequired,
    Linked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsSnapshotV1 {
    pub version: SettingsVersionV1,
    pub settings: SettingsValuesV1,
    pub allowed_voices: Vec<String>,
    pub prompts: PromptTextsV1,
    pub link_status: LinkStatusV1,
    pub bridge_version: String,
    pub restart_pending: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveResultV1 {
    pub version: SettingsVersionV1,
    pub restart_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsErrorV1 {
    pub version: SettingsVersionV1,
    pub message: String,
}
