import { decodeSaved, type PatchV2 } from "./api";
import { saveSettingsV2 } from "./api-v2";
import { element } from "./forms";
import { museReasons, probeMuse } from "./muse-api";
import type { MuseStatusV2 } from "./model";

type Perform = (
    work: (signal: AbortSignal) => Promise<string>,
    message?: string,
    keep?: boolean,
) => Promise<void>;

function labelled<T extends HTMLInputElement | HTMLTextAreaElement>(
    text: string,
    field: T,
) {
    const label = element("label", text);
    label.append(field);
    return label;
}

export function museForm(
    initial: MuseStatusV2,
    actions: string,
    perform: Perform,
) {
    let accepted = initial;
    const form = element("form");
    form.className = "settings-form";
    const status = element("p");
    status.setAttribute("role", "status");
    const url = element("input");
    url.type = "text";
    url.autocomplete = "off";
    url.spellcheck = false;
    url.value = initial.url ?? "";
    const ca = element("textarea");
    ca.rows = 2;
    ca.autocomplete = "off";
    ca.spellcheck = false;
    const credential = element("input");
    credential.type = "password";
    credential.autocomplete = "new-password";
    const save = element("button", "Save Muse");
    save.type = "submit";
    const test = element("button", "Test connection");
    test.type = "button";
    const clear = element("button", "Clear Muse");
    clear.type = "button";
    form.append(
        status,
        labelled("Muse bridge URL", url),
        labelled("Muse CA (base64 DER)", ca),
        labelled("Muse credential", credential),
        save,
        test,
        clear,
    );

    function sync() {
        test.disabled = !accepted.configured || url.value !== accepted.url;
        clear.disabled = !accepted.configured;
        status.textContent = connectionStatus(accepted);
    }
    function forget() {
        ca.value = credential.value = "";
    }
    function submit(patch: NonNullable<PatchV2["muse"]>) {
        void perform(
            async (signal) => {
                const restart = decodeSaved(
                    await saveSettingsV2(actions, signal, { muse: patch }),
                    2,
                );
                if (signal.aborted) return "";
                forget();
                accepted = patch.clear
                    ? { configured: false, reachability: "unchecked" }
                    : {
                          configured: true,
                          url: patch.url
                              ? new URL(patch.url).origin
                              : url.value,
                          reachability: "unchecked",
                      };
                url.value = accepted.url ?? "";
                return restart
                    ? "Saved. Muse changes are waiting for the bridge restart."
                    : "Muse settings saved.";
            },
            "Saving Muse settings…",
            true,
        );
    }
    form.addEventListener("submit", (event) => {
        event.preventDefault();
        const patch: NonNullable<PatchV2["muse"]> = {};
        if (url.value !== (accepted.url ?? "")) patch.url = url.value;
        if (ca.value) patch.ca = ca.value;
        if (credential.value) patch.credential = credential.value;
        if (Object.keys(patch).length) submit(patch);
    });
    url.addEventListener("input", sync);
    test.addEventListener("click", () => {
        if (test.disabled) return;
        void perform(
            async (signal) => {
                const result = await probeMuse(actions, signal);
                if (signal.aborted) return "";
                accepted = {
                    configured: true,
                    url: url.value,
                    reachability: result.reachable
                        ? "reachable"
                        : "unreachable",
                    ...(result.reason ? { reason: result.reason } : {}),
                };
                return "";
            },
            "Testing Muse connection…",
            true,
        );
    });
    clear.addEventListener("click", () => submit({ clear: true }));
    sync();
    return { form, sync, forget };
}

function connectionStatus(status: MuseStatusV2): string {
    if (!status.configured) return "Muse is unconfigured.";
    if (status.reachability === "reachable") return "Muse is reachable.";
    if (status.reason) return museReasons[status.reason];
    return "Muse is configured. Connection unchecked.";
}
