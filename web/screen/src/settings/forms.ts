import type { PatchV2, Snapshot, SnapshotV2 } from "./api";
import { stepper } from "./stepper";

// The largest cap the bridge can store (a TOML integer).
const CAP_MAX = 9223372036854775807n;
let fields = 0;

export function element<K extends keyof HTMLElementTagNameMap>(
    tag: K,
    value = "",
): HTMLElementTagNameMap[K] {
    const node = document.createElement(tag);
    node.textContent = value;
    return node;
}
// One row: the label at the left and its control at the right. `control` may
// wrap `target` (a stepper); the label always names `target` alone.
export function field(
    label: string,
    control: HTMLElement,
    target: HTMLElement = control,
): HTMLDivElement {
    const row = element("div");
    row.className = "settings-row";
    const name = element("label", label);
    target.id = `settings-field-${++fields}`;
    name.htmlFor = target.id;
    row.append(name, control);
    return row;
}
export function number(
    label: string,
    value: number,
    min: number,
    max: number,
    fast: bigint,
) {
    const input = element("input");
    Object.assign(input, {
        type: "number",
        value: String(value),
        min: String(min),
        max: String(max),
        step: "1",
        required: true,
    });
    return {
        input,
        label: field(
            label,
            stepper(input, BigInt(min), BigInt(max), fast),
            input,
        ),
    };
}

export function settingsForm(
    snapshot: Snapshot | SnapshotV2,
    save: (patch: PatchV2) => void,
    extra?: { rows: HTMLElement[]; addPatch: (patch: PatchV2) => void },
) {
    const form = element("form");
    if (extra) form.setAttribute("aria-label", "OpenAI");
    const values = snapshot.settings;
    const enabled = element("input");
    enabled.type = "checkbox";
    enabled.checked = values.voice.session_listen_enabled;
    const seconds = number(
        "Follow-up window (seconds)",
        values.voice.session_listen_seconds,
        3,
        300,
        10n,
    );
    const voice = element("select");
    for (const name of snapshot.allowed_voices) {
        const option = element("option", name);
        option.value = name;
        voice.append(option);
    }
    voice.value = values.codex.voice;
    const cap = element("input");
    Object.assign(cap, {
        type: "text",
        inputMode: "numeric",
        pattern: "0|[1-9][0-9]{0,18}",
        required: true,
        value: values.agent.daily_request_cap,
    });
    const legacyCap =
        snapshot.version === 1 ? snapshot.settings.agent.max_tool_calls : null;
    const tools =
        legacyCap === null
            ? null
            : number("Tool calls per task", legacyCap, 1, 32, 1n);
    form.append(
        ...(extra ? [] : [element("h2", "Conversation")]),
        field("Follow-up listening", enabled),
        seconds.label,
        field("Live voice", voice),
        field("Daily request cap", stepper(cap, 0n, CAP_MAX, 10n), cap),
        ...(tools ? [tools.label] : []),
        ...(extra?.rows ?? []),
        element(
            "p",
            "Settings apply after the current reply finishes. Prompt edits apply to the next conversation.",
        ),
        element("button", "Save settings"),
    );
    form.addEventListener("submit", (event) => {
        event.preventDefault();
        const patch: PatchV2 = {};
        if (enabled.checked !== values.voice.session_listen_enabled)
            (patch.voice ??= {}).session_listen_enabled = enabled.checked;
        if (seconds.input.valueAsNumber !== values.voice.session_listen_seconds)
            (patch.voice ??= {}).session_listen_seconds =
                seconds.input.valueAsNumber;
        if (voice.value !== values.codex.voice)
            patch.codex = { voice: voice.value };
        if (cap.value !== values.agent.daily_request_cap)
            (patch.agent ??= {}).daily_request_cap = cap.value;
        if (tools && tools.input.valueAsNumber !== legacyCap)
            (patch.agent ??= {}).max_tool_calls = tools.input.valueAsNumber;
        extra?.addPatch(patch);
        save(patch);
    });
    return form;
}

export function promptForm(
    kind: "voice" | "runner",
    text: string,
    save: (text: string) => void,
) {
    const form = element("form");
    const input = element("textarea");
    input.value = text;
    input.required = true;
    input.rows = 6;
    input.spellcheck = false;
    const prompt = field(`${kind}.md`, input);
    prompt.classList.add("settings-row-wide");
    form.append(
        element("h2", `${kind === "voice" ? "Voice" : "Runner"} instructions`),
        prompt,
        element("button", `Save ${kind}.md`),
    );
    form.addEventListener("submit", (event) => {
        event.preventDefault();
        if (
            !input.value.trim() ||
            new TextEncoder().encode(input.value).length > 32768
        ) {
            input.setCustomValidity("Use nonblank text of at most 32 KiB.");
            input.reportValidity();
            return;
        }
        save(input.value);
    });
    input.addEventListener("input", () => input.setCustomValidity(""));
    return form;
}
