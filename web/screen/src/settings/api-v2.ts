import {
    assertShape,
    decodeShared,
    decodeSnapshot,
    readResponse,
    record,
    request,
    send,
    settingsUrl,
    snapshotKeys,
    strings,
    type PatchV2,
    type Snapshot,
    type SnapshotV2,
} from "./api";
import type { OpenAiAccountV2, PortosOperationV2 } from "./model";

export async function loadSettings(
    actions: string,
    signal: AbortSignal,
): Promise<Snapshot | SnapshotV2> {
    const response = await send(settingsUrl(actions, "", 2), signal, "GET");
    if (response.status === 404)
        return decodeSnapshot(await request(settingsUrl(actions), signal));
    return decodeV2(await readResponse(response, 2));
}

export async function saveSettingsV2(
    actions: string,
    signal: AbortSignal,
    patch: PatchV2,
) {
    const response = await send(settingsUrl(actions, "", 2), signal, "PATCH", {
        version: 2,
        settings: patch,
    });
    return readResponse(response, 2);
}

export const restartWarning =
    "Could not restart bridge; saved settings remain pending";

export async function readSettingsProgress(
    actions: string,
    signal: AbortSignal,
    waiting: boolean,
) {
    const response = await send(
        settingsUrl(actions, "", 2),
        signal,
        "GET",
    ).catch((error: unknown) => {
        if (waiting && error instanceof TypeError) return undefined;
        throw error;
    });
    if (!response || (waiting && response.status === 503)) return undefined;
    return decodeV2(await readResponse(response, 2));
}

export async function operateAccount(
    actions: string,
    account: "openai" | "portos",
    signal: AbortSignal,
    body: PortosOperationV2,
) {
    const response = await send(
        settingsUrl(actions, "/" + account, 2),
        signal,
        "POST",
        body,
    );
    if (response.status !== 202) {
        await readResponse(response, 2);
        invalid();
    }
    const receipt = record(await response.json());
    assertShape(receipt, ["version", "operation"]);
    if (receipt.version !== 2 || receipt.operation !== body.operation)
        invalid();
}

export async function readOpenAI(actions: string, signal: AbortSignal) {
    const response = await send(
        settingsUrl(actions, "/openai", 2),
        signal,
        "GET",
    );
    if (response.status === 404) return undefined;
    const data = record(await readResponse(response, 2));
    assertShape(
        data,
        ["version", "link_status", "operation_state"],
        ["challenge"],
    );
    if (
        data.version !== 2 ||
        !["checking", "unavailable", "link_required", "linked"].some(
            (status) => status === data.link_status,
        ) ||
        !operationState(data.operation_state)
    )
        invalid();
    if ("challenge" in data) validateChallenge(data.challenge);
    return data as OpenAiAccountV2;
}

function invalid(): never {
    throw new Error("Settings response could not be read");
}

function options(value: unknown, choices: readonly string[]) {
    return strings(value) && value.every((item) => choices.includes(item));
}

export const searchModes = ["live", "cached", "disabled"] as const;
export const groups = ["notes", "prompts"] as const;

function decodeV2(value: unknown): SnapshotV2 {
    const data = record(value);
    const settings = record(data.settings);
    const codex = record(settings.codex);
    const agent = record(settings.agent);
    assertShape(data, [...snapshotKeys, "portos"]);
    assertShape(settings, ["voice", "codex", "agent"], ["home"]);
    assertShape(codex, [
        "voice",
        "delegate_model",
        "delegate_effort",
        "delegate_fast",
        "web_search",
    ]);
    assertShape(agent, [
        "daily_request_cap",
        "max_tool_calls",
        "task_deadline_seconds",
    ]);
    decodeShared(data, codex, agent);
    if (
        data.version !== 2 ||
        typeof codex.delegate_model !== "string" ||
        typeof codex.delegate_effort !== "string" ||
        typeof codex.delegate_fast !== "boolean" ||
        !searchModes.some((mode) => mode === codex.web_search) ||
        !Number.isSafeInteger(agent.task_deadline_seconds)
    )
        invalid();
    if ("home" in settings) {
        const home = record(settings.home);
        assertShape(home, ["agent_tools"]);
        if (!options(home.agent_tools, groups)) invalid();
    }
    validatePortos(data.portos);
    return data as SnapshotV2;
}

function validatePortos(value: unknown) {
    const status = record(value);
    assertShape(
        status,
        [
            "link_status",
            "operation_state",
            "link_waiting_for_restart",
            "scope_choices",
            "tool_choices",
        ],
        ["server", "challenge"],
    );
    if (
        !["unconfigured", "link_required", "linked", "relink_for_scope"].some(
            (state) => state === status.link_status,
        ) ||
        !operationState(status.operation_state) ||
        typeof status.link_waiting_for_restart !== "boolean" ||
        !strings(status.scope_choices) ||
        !strings(status.tool_choices)
    )
        invalid();
    if ("server" in status) {
        const server = record(status.server);
        assertShape(server, [
            "name",
            "url",
            "requested_scopes",
            "granted_scopes",
            "allow_tools",
        ]);
        if (
            typeof server.name !== "string" ||
            typeof server.url !== "string" ||
            !strings(server.requested_scopes) ||
            !strings(server.granted_scopes) ||
            !strings(server.allow_tools)
        )
            invalid();
    }
    if ("challenge" in status) validateChallenge(status.challenge);
}

function operationState(value: unknown) {
    return ["idle", "linking", "unlinking", "expired", "denied", "failed"].some(
        (state) => state === value,
    );
}

function validateChallenge(value: unknown) {
    const challenge = record(value);
    assertShape(challenge, [
        "verification_uri",
        "user_code",
        "expires_at_unix",
    ]);
    if (
        typeof challenge.verification_uri !== "string" ||
        typeof challenge.user_code !== "string" ||
        !Number.isSafeInteger(challenge.expires_at_unix)
    )
        invalid();
}
