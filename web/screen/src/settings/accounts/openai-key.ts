import { saveOpenAIKey } from "../api-v2";
import { actions as formActions, element, field } from "../forms";
import type { OpenAiAccountV2 } from "../model";

type Perform = (
    work: (signal: AbortSignal) => Promise<string>,
) => Promise<unknown>;

// In api_key mode the OpenAI row replaces sign-in with one write-only key
// field. The key is never read back, shown or kept after a save.
export function openAIKeyForm(
    actions: string,
    perform: Perform,
    saved: () => void,
) {
    const form = element("form");
    form.setAttribute("aria-label", "Key replacement");
    form.hidden = true;
    const key = element("input");
    key.type = "password";
    key.autocomplete = "new-password";
    key.spellcheck = false;
    const save = element("button", "Save key");
    save.type = "submit";
    form.append(field("OpenAI API key", key), formActions(save));
    form.addEventListener("submit", (event) => {
        event.preventDefault();
        const value = key.value;
        if (!value) return;
        void perform(async (signal) => {
            try {
                const restart = await saveOpenAIKey(actions, signal, value);
                return restart
                    ? "API key saved. The bridge restarts to use it."
                    : "API key saved.";
            } finally {
                key.value = "";
                if (!signal.aborted) saved();
            }
        });
    });
    function render(status: OpenAiAccountV2 | undefined) {
        form.hidden = !status?.api_key;
    }
    return { form, render };
}

export function apiKeyStatus(status: OpenAiAccountV2): string | undefined {
    if (!status.api_key) return undefined;
    if (!status.api_key.configured) return "Using an API key: no key saved";
    if (status.link_status === "link_required")
        return "Using an API key: not accepted. Save a different key.";
    return "Using an API key: key set";
}
