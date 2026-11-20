import {
    decodeSaved,
    request,
    settingsUrl,
    type PatchV2,
    type Snapshot,
    type SnapshotV2,
} from "./api";
import { element, promptForm, settingsForm } from "./forms";
import { mountKeyboard } from "./keyboard";
import type { UiState } from "../model";
import { packControls } from "./packs";
import { loadSettings, saveSettingsV2 } from "./api-v2";
import { openAIOptions, portosForm } from "./forms-v2";
import { mountPortos } from "./portos/controls";

const REPUBLISHED = "Relink requested. Return to the screen for instructions.";
const LINK_FROM_PC =
    "Link it from the PC with echo-bridge link. Linking on this screen is coming.";
// What Relink can honestly promise: it never starts a sign-in from this page.
const RELINK_RESULT: Record<Snapshot["link_status"], string> = {
    unconfigured: `Port OS is not linked yet. ${LINK_FROM_PC}`,
    link_required: `Port OS needs linking again. ${LINK_FROM_PC}`,
    linked: "Port OS is already linked. Nothing changed.",
};

// Navigation never constructs a ScreenFrame or emits a UiAction. The existing
// presentation continues to accept authoritative SSE updates behind this view.
export function mountSettings(
    root: HTMLElement,
    content: HTMLElement,
    diagnostic: HTMLElement,
    actions: string,
    relink: (signal: AbortSignal) => Promise<void>,
) {
    const gear = element("button", "⚙");
    gear.type = "button";
    gear.className = "settings-gear";
    gear.setAttribute("aria-label", "Settings");
    gear.setAttribute("aria-expanded", "false");
    const panel = element("section");
    panel.className = "settings-panel";
    panel.hidden = true;
    panel.setAttribute("aria-label", "Settings");
    const back = element("button", "Back");
    back.type = "button";
    back.className = "settings-back";
    // The title sits at the left; Back takes the gear's corner at the top right.
    const header = element("div");
    header.className = "settings-header";
    header.append(element("h1", "Settings"), back);
    const status = element("p");
    status.setAttribute("role", "status");
    const muted = element("p", "Microphone muted");
    muted.hidden = true;
    const fields = element("div");
    fields.className = "settings-fields";
    panel.append(header, muted, status, fields);
    root.append(gear, panel);
    const keyboard = mountKeyboard(panel, fields);
    // Set by Relink until the bridge answers with its relink error frame.
    let relinkAsked = false;
    let relinkShown = false;
    let pending: AbortController | undefined;
    let timeout: ReturnType<typeof setTimeout> | undefined;
    let disposed = false;
    let portos: ReturnType<typeof mountPortos> | undefined;

    function close() {
        panel.hidden = true;
        root.classList.remove("settings-open");
        keyboard.close();
        relinkAsked = false;
        content.hidden = diagnostic.hidden = false;
        gear.hidden = false;
        gear.setAttribute("aria-expanded", "false");
        pending?.abort();
        portos?.stop();
        portos = undefined;
        pending = undefined;
        clearTimeout(timeout);
        gear.focus();
    }
    function busy(value: boolean) {
        fields.setAttribute("aria-busy", String(value));
        for (const control of fields.querySelectorAll<
            | HTMLInputElement
            | HTMLButtonElement
            | HTMLSelectElement
            | HTMLTextAreaElement
        >("input,button,select,textarea"))
            control.disabled = value;
    }
    async function perform(
        work: (signal: AbortSignal) => Promise<string>,
        message = "Loading…",
    ) {
        if (pending || disposed || panel.hidden) return;
        const controller = new AbortController();
        const focused = document.activeElement;
        pending = controller;
        busy(true);
        status.textContent = message;
        timeout = setTimeout(() => controller.abort(), 10000);
        try {
            const message = await work(controller.signal);
            if (!controller.signal.aborted && !disposed)
                status.textContent = message;
        } catch (error) {
            if (!panel.hidden && !disposed && pending === controller)
                status.textContent =
                    controller.signal.aborted || !(error instanceof Error)
                        ? "Request could not be confirmed. Reopen Settings to check before saving again."
                        : error.message;
        } finally {
            if (pending === controller) {
                clearTimeout(timeout);
                pending = undefined;
                busy(false);
                if (
                    !disposed &&
                    !panel.hidden &&
                    focused instanceof HTMLElement &&
                    focused.isConnected
                )
                    focused.focus();
                else if (
                    !disposed &&
                    !panel.hidden &&
                    focused instanceof HTMLElement &&
                    !focused.isConnected
                )
                    fields
                        .querySelector<HTMLButtonElement>(
                            ".settings-packs > button",
                        )
                        ?.focus();
            }
        }
    }
    function save(suffix: string, body: unknown, accepted: () => void) {
        void perform(async (signal) => {
            const restarting = decodeSaved(
                await request(settingsUrl(actions, suffix), signal, body),
            );
            if (signal.aborted) return "";
            accepted();
            return restarting
                ? "Saved. The bridge will restart after the current reply finishes."
                : "Saved. Changes apply to the next conversation.";
        }, "Saving…");
    }
    function render(data: Snapshot | SnapshotV2) {
        fields.replaceChildren();
        if (data.version === 1) renderV1(data);
        else renderV2(data);
        for (const warning of data.warnings)
            fields.append(element("p", warning));
    }
    function renderShared(data: Snapshot | SnapshotV2) {
        fields.append(packControls(actions, perform));
        for (const kind of ["voice", "runner"] as const)
            fields.append(
                promptForm(kind, data.prompts[kind], (text) => {
                    save(`/prompts/${kind}`, { version: 1, text }, () => {
                        data.prompts[kind] = text;
                    });
                }),
            );
    }
    function renderV2(data: SnapshotV2) {
        const permissions = portosForm(data.portos, saveV2);
        function saveV2(patch: PatchV2) {
            if (!Object.keys(patch).length) {
                status.textContent = "No settings changed";
                return;
            }
            if (patch.portos?.scopes?.length === 0) {
                status.textContent = "Select at least one requested scope.";
                return;
            }
            void perform(async (signal) => {
                const pending = decodeSaved(
                    await saveSettingsV2(actions, signal, patch),
                    2,
                );
                if (signal.aborted) return "";
                if (patch.voice)
                    Object.assign(data.settings.voice, patch.voice);
                if (patch.codex)
                    Object.assign(data.settings.codex, patch.codex);
                if (patch.agent)
                    Object.assign(data.settings.agent, patch.agent);
                if (patch.home && data.settings.home) {
                    const groups = data.settings.home.agent_tools;
                    groups.splice(0, groups.length, ...patch.home.agent_tools);
                }
                if (patch.portos && data.portos.server) {
                    if (patch.portos.scopes)
                        data.portos.server.requested_scopes.splice(
                            0,
                            data.portos.server.requested_scopes.length,
                            ...patch.portos.scopes,
                        );
                    if (patch.portos.scopes && pending) portos?.scopesSaved();
                    if (patch.portos.allow_tools)
                        data.portos.server.allow_tools.splice(
                            0,
                            data.portos.server.allow_tools.length,
                            ...patch.portos.allow_tools,
                        );
                }
                return pending
                    ? "Saved. The bridge will restart after the current reply finishes."
                    : "Saved. Changes apply to the next conversation.";
            }, "Saving�");
        }
        portos = mountPortos(
            permissions.controls,
            actions,
            data.portos,
            perform,
            data.warnings.includes(
                "Could not restart bridge; saved settings remain pending",
            ),
        );
        fields.append(
            settingsForm(data, saveV2, openAIOptions(data.settings)),
            permissions.section,
        );
        renderShared(data);
        fields.append(element("p", "Version: " + data.bridge_version));
    }
    function renderV1(data: Snapshot) {
        const form = settingsForm(data, (patch) => {
            if (!Object.keys(patch).length) {
                status.textContent = "No settings changed";
                return;
            }
            save("", { version: 1, settings: patch }, () => {
                if (patch.voice)
                    Object.assign(data.settings.voice, patch.voice);
                if (patch.codex)
                    Object.assign(data.settings.codex, patch.codex);
                if (patch.agent)
                    Object.assign(data.settings.agent, patch.agent);
            });
        });
        fields.append(form);
        renderShared(data);
        fields.append(
            element("h2", "Bridge"),
            element("p", `Port OS: ${data.link_status.replace(/_/g, " ")}`),
            element("p", `Version: ${data.bridge_version}`),
        );
        const link = element("button", "Relink");
        link.type = "button";
        link.addEventListener("click", () => {
            void perform(async (signal) => {
                relinkAsked = true;
                relinkShown = false;
                await relink(signal);
                return relinkShown
                    ? REPUBLISHED
                    : RELINK_RESULT[data.link_status];
            });
        });
        fields.append(link);
    }
    gear.addEventListener("click", () => {
        if (disposed) return;
        panel.hidden = false;
        root.classList.add("settings-open");
        keyboard.close();
        content.hidden = diagnostic.hidden = true;
        gear.hidden = true;
        gear.setAttribute("aria-expanded", "true");
        fields.replaceChildren();
        back.focus();
        void perform(async (signal) => {
            const data = await loadSettings(actions, signal);
            if (signal.aborted) return "";
            render(data);
            return data.restart_pending
                ? "Saved settings are waiting for a bridge restart."
                : "Settings loaded";
        });
    });
    back.addEventListener("click", close);
    panel.addEventListener("keydown", (event) => {
        if (event.key === "Escape") close();
    });
    return {
        update(state: UiState) {
            muted.hidden = state.state !== "muted";
            // The bridge republishes its relink error only when a relink is
            // really required; that frame is the one honest "it happened".
            if (relinkAsked && state.state === "error" && state.relink) {
                relinkAsked = false;
                relinkShown = true;
                if (!pending) status.textContent = REPUBLISHED;
            }
        },
        dispose() {
            disposed = true;
            pending?.abort();
            portos?.stop();
            clearTimeout(timeout);
            gear.remove();
            panel.remove();
            root.classList.remove("settings-open");
            content.hidden = diagnostic.hidden = false;
        },
    };
}
