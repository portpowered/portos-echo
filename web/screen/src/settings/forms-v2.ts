import { groups, searchModes, speeds } from "./api-v2";
import { actions, element, field } from "./forms";
import type { Save, Track } from "./forms";
import type { PatchV2, SnapshotV2 } from "./api";

function choices<T extends string>(
    label: string,
    items: readonly T[],
    current: string[],
) {
    const fieldset = element("fieldset");
    fieldset.className = "settings-choices";
    fieldset.append(element("legend", label));
    const inputs = items.map((name) => {
        const input = element("input");
        input.type = "checkbox";
        input.checked = current.includes(name);
        const choice = element("label");
        choice.append(input, element("span", name));
        fieldset.append(choice);
        return { name, input };
    });
    const selected = () =>
        inputs.filter(({ input }) => input.checked).map(({ name }) => name);
    return {
        fieldset,
        changed: () =>
            inputs.some(
                ({ name, input }) => input.checked !== current.includes(name),
            ),
        selected,
    };
}

const speedLabels = {
    standard: "Standard",
    fast: "Fast",
    ultrafast: "Ultrafast",
} as const;
const SPEED_HINT =
    "Ultrafast needs GPT-6 Astra or GPT-6.1 Sol and costs more. Unsupported models run at standard.";

export function openAIOptions(settings: SnapshotV2["settings"]) {
    const { codex, home } = settings;
    const search = element("select");
    for (const mode of searchModes) search.append(element("option", mode));
    search.value = codex.web_search;
    const model = element("input");
    model.value = codex.delegate_model;
    model.placeholder = "Pin default";
    const effort = element("input");
    effort.value = codex.delegate_effort;
    effort.placeholder = "Pin default";
    const speed = element("select");
    for (const tier of speeds) {
        const option = element("option", speedLabels[tier]);
        option.value = tier;
        speed.append(option);
    }
    speed.value = codex.delegate_speed;
    const speedHint = element("p", SPEED_HINT);
    speedHint.className = "settings-hint";
    const local =
        home && choices("Local agent tools", groups, home.agent_tools);
    return {
        rows: [
            field("Web search", search),
            field("Delegated model", model),
            field("Delegated reasoning effort", effort),
            field("Delegated speed", speed),
            speedHint,
            local?.fieldset ?? element("p", "Local agent tools unavailable"),
        ],
        addPatch(patch: PatchV2) {
            const mode = searchModes.find((mode) => mode === search.value);
            if (mode && mode !== codex.web_search)
                (patch.codex ??= {}).web_search = mode;
            if (model.value !== codex.delegate_model)
                (patch.codex ??= {}).delegate_model = model.value;
            if (effort.value !== codex.delegate_effort)
                (patch.codex ??= {}).delegate_effort = effort.value;
            const tier = speeds.find((tier) => tier === speed.value);
            if (tier && tier !== codex.delegate_speed)
                (patch.codex ??= {}).delegate_speed = tier;
            if (local?.changed())
                patch.home = { agent_tools: local.selected() };
        },
    };
}

export function portosForm(
    status: SnapshotV2["portos"],
    save: Save<PatchV2>,
    track: Track,
) {
    const section = element("section");
    const server = status.server;
    if (!server) return section;
    const form = element("form");
    const scopes = choices(
        "Requested scopes",
        status.scope_choices,
        server.requested_scopes,
    );
    const tools = choices(
        "Allowed tools",
        status.tool_choices,
        server.allow_tools,
    );
    form.append(
        element("p", server.name),
        element("p", server.url),
        scopes.fieldset,
        tools.fieldset,
        actions(element("button", "Save Port OS permissions")),
    );
    for (const [label, current, full] of [
        [
            "Other requested scopes",
            server.requested_scopes,
            status.scope_choices,
        ],
        ["Other allowed tools", server.allow_tools, status.tool_choices],
    ] as const) {
        const custom = current.filter(
            (name) => !(full as readonly string[]).includes(name),
        );
        if (custom.length)
            form.append(element("p", label + ": " + custom.join(", ")));
    }
    function submit() {
        const patch: NonNullable<PatchV2["portos"]> = {};
        if (scopes.changed()) patch.scopes = scopes.selected();
        if (tools.changed()) patch.allow_tools = tools.selected();
        return save(Object.keys(patch).length ? { portos: patch } : {});
    }
    form.addEventListener("submit", (event) => {
        event.preventDefault();
        void submit();
    });
    track({
        dirty: () => scopes.changed() || tools.changed(),
        save: submit,
    });
    section.append(form);
    return section;
}
