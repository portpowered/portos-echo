import { groups, searchModes } from "./api-v2";
import { element, field, number } from "./forms";
import type { PatchV2, SnapshotV2 } from "./api";

function choices<T extends string>(label: string, items: readonly T[], current: string[]) {
    const fieldset = element("fieldset");
    fieldset.append(element("legend", label));
    const inputs = items.map((name) => {
        const input = element("input");
        input.type = "checkbox";
        input.checked = current.includes(name);
        fieldset.append(field(name, input));
        return { name, input };
    });
    const selected = () => inputs.filter(({input}) => input.checked).map(({name}) => name);
    return {
        fieldset,
        changed: () => inputs.some(({name, input}) => input.checked !== current.includes(name)),
        selected,
    };
}

export function openAIOptions(settings: SnapshotV2["settings"]) {
    const { codex, agent, home } = settings;
    const search = element("select");
    for (const mode of searchModes)
        search.append(element("option", mode));
    search.value = codex.web_search;
    const model = element("input");
    model.value = codex.delegate_model;
    model.placeholder = "Pin default";
    const effort = element("input");
    effort.value = codex.delegate_effort;
    effort.placeholder = "Pin default";
    const deadline = number("Task deadline (seconds)", agent.task_deadline_seconds, 5, 600, 10n);
    const local = home && choices("Local agent tools", groups, home.agent_tools);
    return {
        rows: [
            field("Web search", search),
            field("Delegated model", model),
            field("Delegated reasoning effort", effort),
            deadline.label,
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
            if (deadline.input.valueAsNumber !== agent.task_deadline_seconds)
                (patch.agent ??= {}).task_deadline_seconds = deadline.input.valueAsNumber;
            if (local?.changed()) patch.home = { agent_tools: local.selected() };
        },
    };
}

const statusLabels: Record<SnapshotV2["portos"]["link_status"], string> = {
    unconfigured: "Unconfigured",
    link_required: "Link required",
    linked: "Linked",
    relink_for_scope: "Relink needed for scope",
};

export function portosForm(status: SnapshotV2["portos"], save: (patch: PatchV2) => void) {
    const section = element("section");
    section.setAttribute("aria-label", "Port OS");
    section.append(element("h2", "Port OS"), element("p", statusLabels[status.link_status]));
    const server = status.server;
    if (!server) return section;
    const form = element("form");
    const scopes = choices("Requested scopes", status.scope_choices, server.requested_scopes);
    const tools = choices("Allowed tools", status.tool_choices, server.allow_tools);
    form.append(
        element("p", server.name),
        element("p", server.url),
        element("p", "Granted scopes: " + (server.granted_scopes.join(", ") || "None")),
        scopes.fieldset,
        tools.fieldset,
        element("button", "Save Port OS permissions"),
    );
    for (const [label, current, full] of [
        ["Other requested scopes", server.requested_scopes, status.scope_choices],
        ["Other allowed tools", server.allow_tools, status.tool_choices],
    ] as const) {
        const custom = current.filter((name) => !(full as readonly string[]).includes(name));
        if (custom.length) form.append(element("p", label + ": " + custom.join(", ")));
    }
    form.addEventListener("submit", (event) => {
        event.preventDefault();
        const patch: NonNullable<PatchV2["portos"]> = {};
        if (scopes.changed()) patch.scopes = scopes.selected();
        if (tools.changed()) patch.allow_tools = tools.selected();
        save(Object.keys(patch).length ? {portos: patch} : {});
    });
    section.append(form);
    return section;
}
