import {
    decodeSaved,
    decodeSnapshot,
    request,
    settingsUrl,
    type Snapshot,
} from "./api";
import { element, promptForm, settingsForm } from "./forms";
import type { UiState } from "../model";
import { packControls } from "./packs";

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
    const status = element("p");
    status.setAttribute("role", "status");
    const muted = element("p", "Microphone muted");
    muted.hidden = true;
    const fields = element("div");
    fields.className = "settings-fields";
    panel.append(back, element("h1", "Settings"), muted, status, fields);
    root.append(gear, panel);
    let pending: AbortController | undefined;
    let timeout: ReturnType<typeof setTimeout> | undefined;
    let disposed = false;

    function close() {
        panel.hidden = true;
        content.hidden = diagnostic.hidden = false;
        gear.hidden = false;
        gear.setAttribute("aria-expanded", "false");
        pending?.abort();
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
    function render(data: Snapshot) {
        fields.replaceChildren();
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
        fields.append(form, packControls(actions, perform));
        for (const kind of ["voice", "runner"] as const)
            fields.append(
                promptForm(kind, data.prompts[kind], (text) => {
                    save(`/prompts/${kind}`, { version: 1, text }, () => {
                        data.prompts[kind] = text;
                    });
                }),
            );
        fields.append(
            element("h2", "Bridge"),
            element("p", `Port OS: ${data.link_status.replace(/_/g, " ")}`),
            element("p", `Version: ${data.bridge_version}`),
        );
        const link = element("button", "Relink");
        link.type = "button";
        link.addEventListener("click", () => {
            void perform(async (signal) => {
                await relink(signal);
                return "Relink requested. Return to the screen for instructions.";
            });
        });
        fields.append(link);
        for (const warning of data.warnings)
            fields.append(element("p", warning));
    }
    gear.addEventListener("click", () => {
        if (disposed) return;
        panel.hidden = false;
        content.hidden = diagnostic.hidden = true;
        gear.hidden = true;
        gear.setAttribute("aria-expanded", "true");
        fields.replaceChildren();
        back.focus();
        void perform(async (signal) => {
            const data = decodeSnapshot(
                await request(settingsUrl(actions), signal),
            );
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
        },
        dispose() {
            disposed = true;
            pending?.abort();
            clearTimeout(timeout);
            gear.remove();
            panel.remove();
            content.hidden = diagnostic.hidden = false;
        },
    };
}
