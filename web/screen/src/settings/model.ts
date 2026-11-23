// Generated from schemas/screen/*_v{1,2}.json by tools/screen-schema.mjs.
// Schema SHA-256: 3e560368c861921ed8b062de1ebae3e52f630e9c950f73fa6e18094ba5a34be3
export type AgentValuesV1 = {
    daily_request_cap: DailyCapV1;
    max_tool_calls: number;
};
export type CodexPatchV1 = { voice: string };
export type DailyCapV1 = string;
export type LinkStatusV1 = "unconfigured" | "link_required" | "linked";
export type PromptTextsV1 = { voice: string; runner: string };
export type SettingsValuesV1 = {
    voice: VoiceValuesV1;
    codex: CodexPatchV1;
    agent: AgentValuesV1;
};
export type SettingsVersionV1 = 1;
export type VoiceValuesV1 = {
    session_listen_enabled: boolean;
    session_listen_seconds: number;
};
export type SettingsSnapshotV1 = {
    version: SettingsVersionV1;
    settings: SettingsValuesV1;
    allowed_voices: Array<string>;
    prompts: PromptTextsV1;
    link_status: LinkStatusV1;
    bridge_version: string;
    restart_pending: boolean;
    warnings: Array<string>;
};
export type AgentPatchV1 = {
    daily_request_cap?: DailyCapV1;
    max_tool_calls?: number;
};
export type SettingsFieldsV1 = {
    agent?: AgentPatchV1;
    codex?: CodexPatchV1;
    voice?: VoicePatchV1;
};
export type VoicePatchV1 = {
    session_listen_enabled?: boolean;
    session_listen_seconds?: number;
};
export type SettingsPatchV1 = {
    version: SettingsVersionV1;
    settings: SettingsFieldsV1;
};
export type PromptSaveV1 = { version: SettingsVersionV1; text: string };
export type SaveResultV1 = {
    version: SettingsVersionV1;
    restart_pending: boolean;
};
export type SettingsErrorV1 = { version: SettingsVersionV1; message: string };
export type AgentValuesV2 = { daily_request_cap: DailyCapV1 };
export type CodexValuesV2 = {
    voice: string;
    web_search: WebSearchV2;
    delegate_model: string;
    delegate_effort: string;
    delegate_fast: boolean;
};
export type HomeSettingsV2 = { agent_tools: Array<HomeToolGroupV2> };
export type HomeToolGroupV2 = "notes" | "prompts";
export type LinkStatusV2 =
    "unconfigured" | "link_required" | "linked" | "relink_for_scope";
export type MuseProbeReasonV2 =
    | "unconfigured"
    | "invalid_configuration"
    | "unauthorized"
    | "unavailable"
    | "unexpected_response";
export type MuseReachabilityV2 = "unchecked" | "reachable" | "unreachable";
export type MuseStatusV2 = {
    configured: boolean;
    reachability: MuseReachabilityV2;
    reason?: MuseProbeReasonV2;
    url?: string;
};
export type PortosChallengeV2 = {
    verification_uri: string;
    user_code: string;
    expires_at_unix: number;
};
export type PortosOperationStateV2 =
    "idle" | "linking" | "unlinking" | "expired" | "denied" | "failed";
export type PortosScopeV2 =
    | "endpoint:read"
    | "endpoint:write"
    | "message:send"
    | "group:read"
    | "group:manage"
    | "subscription:read"
    | "subscription:write";
export type PortosServerV2 = {
    name: string;
    url: string;
    requested_scopes: Array<string>;
    granted_scopes: Array<string>;
    allow_tools: Array<string>;
};
export type PortosStatusV2 = {
    link_status: LinkStatusV2;
    operation_state: PortosOperationStateV2;
    link_waiting_for_restart: boolean;
    scope_choices: Array<PortosScopeV2>;
    tool_choices: Array<PortosToolV2>;
    challenge?: PortosChallengeV2;
    endpoint_enabled?: boolean;
    endpoint_name?: string;
    relink_advised?: boolean;
    server?: PortosServerV2;
};
export type PortosToolV2 =
    | "control"
    | "endpoint_query"
    | "endpoint_show"
    | "endpoint_manage"
    | "group_query"
    | "group_manage"
    | "guide"
    | "subscription_list"
    | "subscription_inspect"
    | "subscription_cancel"
    | "subscription_label_edit";
export type SettingsValuesV2 = {
    voice: VoiceValuesV1;
    codex: CodexValuesV2;
    agent: AgentValuesV2;
    home?: HomeSettingsV2;
};
export type SettingsVersionV2 = 2;
export type WebSearchV2 = "live" | "cached" | "disabled";
export type SettingsSnapshotV2 = {
    version: SettingsVersionV2;
    settings: SettingsValuesV2;
    allowed_voices: Array<string>;
    prompts: PromptTextsV1;
    portos: PortosStatusV2;
    bridge_version: string;
    restart_pending: boolean;
    warnings: Array<string>;
    muse?: MuseStatusV2;
};
export type AgentPatchV2 = {
    daily_request_cap?: DailyCapV1;
    max_tool_calls?: number;
    task_deadline_seconds?: number;
};
export type CodexPatchV2 = {
    delegate_effort?: string;
    delegate_fast?: boolean;
    delegate_model?: string;
    voice?: string;
    web_search?: WebSearchV2;
};
export type MusePatchV2 = {
    ca?: string;
    clear?: true;
    credential?: string;
    url?: string;
};
export type PortosPermissionsPatchV2 = {
    allow_tools?: Array<PortosToolV2>;
    endpoint_enabled?: boolean;
    endpoint_name?: string;
    scopes?: Array<PortosScopeV2>;
};
export type SettingsFieldsV2 = {
    agent?: AgentPatchV2;
    codex?: CodexPatchV2;
    home?: HomeSettingsV2;
    muse?: MusePatchV2;
    portos?: PortosPermissionsPatchV2;
    voice?: VoicePatchV1;
};
export type SettingsPatchV2 = {
    version: SettingsVersionV2;
    settings: SettingsFieldsV2;
};
export type SaveResultV2 = {
    version: SettingsVersionV2;
    restart_pending: boolean;
};
export type SettingsErrorV2 = { version: SettingsVersionV2; message: string };
export type ConfirmUnlink = true;
export type PortosOperationV2 =
    | { operation: "link"; version: SettingsVersionV2 }
    | {
          operation: "unlink";
          version: SettingsVersionV2;
          confirm: ConfirmUnlink;
      };
export type PortosActionV2 = "link" | "unlink";
export type PortosOperationStartedV2 = {
    version: SettingsVersionV2;
    operation: PortosActionV2;
};
export type OpenAiLinkStatusV2 =
    "checking" | "unavailable" | "link_required" | "linked";
export type OpenAiAccountV2 = {
    version: SettingsVersionV2;
    link_status: OpenAiLinkStatusV2;
    operation_state: PortosOperationStateV2;
    challenge?: PortosChallengeV2;
    api_key?: OpenAiApiKeyStatusV2;
};
export type OpenAiApiKeyStatusV2 = { configured: boolean };
export type OpenAiApiKeyRequestV2 = {
    version: SettingsVersionV2;
    api_key: string;
};
export type MuseProbeRequestV2 = {
    operation: "probe";
    version: SettingsVersionV2;
};
export type MuseProbeResultV2 = {
    version: SettingsVersionV2;
    reachable: boolean;
    reason?: MuseProbeReasonV2;
};
