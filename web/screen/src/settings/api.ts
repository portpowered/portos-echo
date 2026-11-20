// Versioned settings HTTP adapter. Settings never share the SSE integer decoder or action body.
import type {
    SettingsPatchV1,
    SettingsPatchV2,
    SettingsSnapshotV1,
    SettingsSnapshotV2,
} from "./model";
export type Snapshot = SettingsSnapshotV1;
export type Values = Snapshot["settings"];
export type Patch = SettingsPatchV1["settings"];

export type SnapshotV2 = SettingsSnapshotV2;
export type PatchV2 = SettingsPatchV2["settings"];

export function settingsUrl(actions: string, suffix = "", version: 1 | 2 = 1): string {
    const url = new URL(actions);
    const tokens = url.searchParams.getAll("token");
    if (tokens.length !== 1 || !tokens[0])
        throw new Error("Settings configuration is invalid");
    const endpoint = new URL(`/settings/v${version}${suffix}`, url.origin);
    endpoint.searchParams.set("token", tokens[0]);
    return endpoint.href;
}

export function record(value: unknown): Record<string, unknown> {
    if (!value || typeof value !== "object" || Array.isArray(value))
        throw new Error("Settings response could not be read");
    return value as Record<string, unknown>;
}
export function strings(value: unknown): value is string[] {
    return (
        Array.isArray(value) && value.every((item) => typeof item === "string")
    );
}

export function assertShape(
    value: Record<string, unknown>,
    required: string[],
    optional: string[] = [],
) {
    if (
        required.some((key) => !Object.prototype.hasOwnProperty.call(value, key)) ||
        Object.keys(value).some((key) => !required.includes(key) && !optional.includes(key))
    )
        invalid();
}

function invalid(): never {
    throw new Error("Settings response could not be read");
}

export const snapshotKeys = [
    "version", "settings", "allowed_voices", "prompts", "bridge_version",
    "restart_pending", "warnings",
];

export function decodeShared(data: Record<string, unknown>, codex: Record<string, unknown>, agent: Record<string, unknown>) {
    const voice = record(record(data.settings).voice);
    const prompts = record(data.prompts);
    assertShape(voice, ["session_listen_enabled", "session_listen_seconds"]);
    assertShape(prompts, ["voice", "runner"]);
    if (
        typeof voice.session_listen_enabled !== "boolean" ||
        !Number.isSafeInteger(voice.session_listen_seconds) ||
        typeof codex.voice !== "string" ||
        typeof agent.daily_request_cap !== "string" ||
        !/^(0|[1-9][0-9]{0,18})$/.test(agent.daily_request_cap) ||
        !Number.isSafeInteger(agent.max_tool_calls) ||
        !strings(data.allowed_voices) ||
        !data.allowed_voices.includes(codex.voice) ||
        typeof prompts.voice !== "string" ||
        typeof prompts.runner !== "string" ||
        typeof data.bridge_version !== "string" ||
        typeof data.restart_pending !== "boolean" ||
        !strings(data.warnings)
    )
        invalid();
}

export function decodeSnapshot(value: unknown): Snapshot {
    const data = record(value);
    const settings = record(data.settings);
    const codex = record(settings.codex);
    const agent = record(settings.agent);
    assertShape(data, [...snapshotKeys, "link_status"]);
    assertShape(settings, ["voice", "codex", "agent"]);
    assertShape(codex, ["voice"]);
    assertShape(agent, ["daily_request_cap", "max_tool_calls"]);
    decodeShared(data, codex, agent);
    if (
        data.version !== 1 ||
        !["unconfigured", "link_required", "linked"].some((status) => status === data.link_status)
    )
        invalid();
    return data as Snapshot;
}

export async function request(
    url: string,
    signal: AbortSignal,
    body?: unknown,
): Promise<unknown> {
    return readResponse(await send(url, signal, body === undefined ? "GET" : "POST", body), 1);
}

export async function send(
    url: string,
    signal: AbortSignal,
    method: string,
    body?: unknown,
): Promise<Response> {
    return fetch(url, {
        method,
        ...(body === undefined
            ? {}
            : {
                  headers: { "Content-Type": "application/json" },
                  body: JSON.stringify(body),
              }),
        signal,
        cache: "no-store",
        credentials: "omit",
        referrerPolicy: "no-referrer",
    });
}

export async function readResponse(response: Response, version: 1 | 2): Promise<unknown> {
    if (response.status === 200) return response.json();
    if (response.status === 422 || response.status === 500) {
        const error = record(await response.json());
        if (error.version === version && typeof error.message === "string")
            throw new Error(error.message);
    }
    if (response.status === 401 || response.status === 403)
        throw new Error("Pairing is unavailable. Reopen the paired screen.");
    if (response.status === 503)
        throw new Error(
            "Settings are unavailable. Reopen Settings to try again.",
        );
    throw new Error("Settings request was refused");
}

export function decodeSaved(value: unknown, version: 1 | 2 = 1): boolean {
    const data = record(value);
    if (
        Object.keys(data).length !== 2 ||
        data.version !== version ||
        typeof data.restart_pending !== "boolean"
    )
        throw new Error("Save response could not be read");
    return data.restart_pending;
}
