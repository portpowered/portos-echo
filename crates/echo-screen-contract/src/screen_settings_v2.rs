//! Version two runs beside the strict V1 settings wire.
use super::settings_v1::{DailyCapV1, PromptTextsV1, VoicePatchV1, VoiceValuesV1, present};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct SettingsVersionV2;

impl TryFrom<u8> for SettingsVersionV2 {
    type Error = &'static str;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value == 2 {
            Ok(Self)
        } else {
            Err("unsupported settings version")
        }
    }
}

impl From<SettingsVersionV2> for u8 {
    fn from(_: SettingsVersionV2) -> Self {
        2
    }
}

impl JsonSchema for SettingsVersionV2 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SettingsVersionV2".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type": "integer", "const": 2})
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum WebSearchV2 {
    #[serde(rename = "live")]
    Live,
    #[serde(rename = "cached")]
    Cached,
    #[serde(rename = "disabled")]
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum HomeToolGroupV2 {
    #[serde(rename = "notes")]
    Notes,
    #[serde(rename = "prompts")]
    Prompts,
}

impl HomeToolGroupV2 {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Notes => "notes",
            Self::Prompts => "prompts",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum PortosScopeV2 {
    #[serde(rename = "endpoint:read")]
    EndpointRead,
    #[serde(rename = "endpoint:write")]
    EndpointWrite,
    #[serde(rename = "message:send")]
    MessageSend,
    #[serde(rename = "group:read")]
    GroupRead,
    #[serde(rename = "group:manage")]
    GroupManage,
    #[serde(rename = "subscription:read")]
    SubscriptionRead,
    #[serde(rename = "subscription:write")]
    SubscriptionWrite,
}

impl PortosScopeV2 {
    pub const ALL: [Self; 7] = [
        Self::EndpointRead,
        Self::EndpointWrite,
        Self::MessageSend,
        Self::GroupRead,
        Self::GroupManage,
        Self::SubscriptionRead,
        Self::SubscriptionWrite,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EndpointRead => "endpoint:read",
            Self::EndpointWrite => "endpoint:write",
            Self::MessageSend => "message:send",
            Self::GroupRead => "group:read",
            Self::GroupManage => "group:manage",
            Self::SubscriptionRead => "subscription:read",
            Self::SubscriptionWrite => "subscription:write",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum PortosToolV2 {
    #[serde(rename = "control")]
    Control,
    #[serde(rename = "endpoint_query")]
    EndpointQuery,
    #[serde(rename = "endpoint_show")]
    EndpointShow,
    #[serde(rename = "endpoint_manage")]
    EndpointManage,
    #[serde(rename = "group_query")]
    GroupQuery,
    #[serde(rename = "group_manage")]
    GroupManage,
    #[serde(rename = "guide")]
    Guide,
    #[serde(rename = "subscription_list")]
    SubscriptionList,
    #[serde(rename = "subscription_inspect")]
    SubscriptionInspect,
    #[serde(rename = "subscription_cancel")]
    SubscriptionCancel,
    #[serde(rename = "subscription_label_edit")]
    SubscriptionLabelEdit,
}

impl PortosToolV2 {
    pub const ALL: [Self; 11] = [
        Self::Control,
        Self::EndpointQuery,
        Self::EndpointShow,
        Self::EndpointManage,
        Self::GroupQuery,
        Self::GroupManage,
        Self::Guide,
        Self::SubscriptionList,
        Self::SubscriptionInspect,
        Self::SubscriptionCancel,
        Self::SubscriptionLabelEdit,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::EndpointQuery => "endpoint_query",
            Self::EndpointShow => "endpoint_show",
            Self::EndpointManage => "endpoint_manage",
            Self::GroupQuery => "group_query",
            Self::GroupManage => "group_manage",
            Self::Guide => "guide",
            Self::SubscriptionList => "subscription_list",
            Self::SubscriptionInspect => "subscription_inspect",
            Self::SubscriptionCancel => "subscription_cancel",
            Self::SubscriptionLabelEdit => "subscription_label_edit",
        }
    }
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
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("minProperties" = 1))]
pub struct AgentPatchV2 {
    #[schemars(with = "DailyCapV1")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub daily_request_cap: Option<DailyCapV1>,
    #[schemars(with = "u32", range(min = 1, max = 32))]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub max_tool_calls: Option<u32>,
    #[schemars(with = "u32", range(min = 5, max = 600))]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub task_deadline_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HomeSettingsV2 {
    #[schemars(extend("uniqueItems" = true))]
    pub agent_tools: Vec<HomeToolGroupV2>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("minProperties" = 1))]
pub struct PortosPermissionsPatchV2 {
    #[schemars(with = "Vec<PortosScopeV2>", length(min = 1), extend("uniqueItems" = true))]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub scopes: Option<Vec<PortosScopeV2>>,
    #[schemars(with = "Vec<PortosToolV2>", extend("uniqueItems" = true))]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub allow_tools: Option<Vec<PortosToolV2>>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("minProperties" = 1))]
pub struct SettingsFieldsV2 {
    #[schemars(with = "VoicePatchV1")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub voice: Option<VoicePatchV1>,
    #[schemars(with = "CodexPatchV2")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub codex: Option<CodexPatchV2>,
    #[schemars(with = "AgentPatchV2")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub agent: Option<AgentPatchV2>,
    #[schemars(with = "HomeSettingsV2")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub home: Option<HomeSettingsV2>,
    #[schemars(with = "PortosPermissionsPatchV2")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub portos: Option<PortosPermissionsPatchV2>,
}

impl SettingsFieldsV2 {
    /// Check supplied sections before handing off to config validation.
    #[must_use]
    pub fn valid_shape(&self) -> bool {
        self != &Self::default()
            && self
                .voice
                .as_ref()
                .is_none_or(|voice| voice != &VoicePatchV1::default())
            && self
                .codex
                .as_ref()
                .is_none_or(|codex| codex != &CodexPatchV2::default())
            && self
                .agent
                .as_ref()
                .is_none_or(|agent| agent != &AgentPatchV2::default())
            && self.home.as_ref().is_none_or(|home| distinct(&home.agent_tools))
            && self.portos.as_ref().is_none_or(PortosPermissionsPatchV2::valid_shape)
    }
}

impl PortosPermissionsPatchV2 {
    fn valid_shape(&self) -> bool {
        self != &Self::default()
            && self
                .scopes
                .as_ref()
                .is_none_or(|scopes| !scopes.is_empty() && distinct(scopes))
            && self.allow_tools.as_ref().is_none_or(|tools| distinct(tools))
    }
}

fn distinct<T: PartialEq>(items: &[T]) -> bool {
    items.iter().enumerate().all(|(i, item)| !items[..i].contains(item))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatchV2 {
    pub version: SettingsVersionV2,
    pub settings: SettingsFieldsV2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodexValuesV2 {
    pub voice: String,
    pub web_search: WebSearchV2,
    pub delegate_model: String,
    pub delegate_effort: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentValuesV2 {
    pub daily_request_cap: DailyCapV1,
    #[schemars(range(min = 1, max = 32))]
    pub max_tool_calls: u32,
    #[schemars(with = "u32", range(min = 5, max = 600))]
    pub task_deadline_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsValuesV2 {
    pub voice: VoiceValuesV1,
    pub codex: CodexValuesV2,
    pub agent: AgentValuesV2,
    #[schemars(with = "HomeSettingsV2")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub home: Option<HomeSettingsV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PortosServerV2 {
    pub name: String,
    #[schemars(url)]
    pub url: String,
    pub requested_scopes: Vec<String>,
    pub granted_scopes: Vec<String>,
    pub allow_tools: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkStatusV2 {
    Unconfigured,
    LinkRequired,
    Linked,
    RelinkForScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PortosOperationStateV2 {
    Idle,
    Linking,
    Unlinking,
    Expired,
    Denied,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PortosChallengeV2 {
    #[schemars(url)]
    pub verification_uri: String,
    pub user_code: String,
    #[schemars(with = "usize")]
    pub expires_at_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PortosStatusV2 {
    pub link_status: LinkStatusV2,
    pub operation_state: PortosOperationStateV2,
    pub link_waiting_for_restart: bool,
    pub scope_choices: Vec<PortosScopeV2>,
    pub tool_choices: Vec<PortosToolV2>,
    #[schemars(with = "PortosServerV2")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub server: Option<PortosServerV2>,
    #[schemars(with = "PortosChallengeV2")]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub challenge: Option<PortosChallengeV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsSnapshotV2 {
    pub version: SettingsVersionV2,
    pub settings: SettingsValuesV2,
    pub allowed_voices: Vec<String>,
    pub prompts: PromptTextsV1,
    pub portos: PortosStatusV2,
    pub bridge_version: String,
    pub restart_pending: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveResultV2 {
    pub version: SettingsVersionV2,
    pub restart_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsErrorV2 {
    pub version: SettingsVersionV2,
    pub message: String,
}

/// An unlink requires explicit acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "bool", into = "bool")]
pub struct ConfirmUnlink;

impl TryFrom<bool> for ConfirmUnlink {
    type Error = &'static str;

    fn try_from(value: bool) -> Result<Self, Self::Error> {
        value.then_some(Self).ok_or("unlink requires confirmation")
    }
}

impl From<ConfirmUnlink> for bool {
    fn from(_: ConfirmUnlink) -> Self {
        true
    }
}

impl JsonSchema for ConfirmUnlink {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ConfirmUnlink".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type": "boolean", "const": true})
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum PortosOperationV2 {
    Link { version: SettingsVersionV2 },
    Unlink {
        version: SettingsVersionV2,
        confirm: ConfirmUnlink,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PortosActionV2 {
    Link,
    Unlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PortosOperationStartedV2 {
    pub version: SettingsVersionV2,
    pub operation: PortosActionV2,
}
