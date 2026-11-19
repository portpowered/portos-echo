// Generated from schemas/screen/*_v1.json by tools/screen-schema.mjs.
// Schema SHA-256: 9ac5c185d924a33e682e69333f6c2f1686623249c3cfcfd937c1f8f381aa4765
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
