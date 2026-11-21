import {
    endpointConnecting,
    endpointMessages,
    operateAccount,
    readSettingsProgress,
    restartWarning,
    validEndpointName,
} from "../api-v2";
import type { PatchV2, SnapshotV2 } from "../api";
import {
    actions as formActions,
    element,
    field,
    type Save,
    type Track,
} from "../forms";
import type { PortosOperationV2 } from "../model";
import {
    accountButtons,
    accountChallenge,
    type AccountRow,
} from "./account-buttons";

type Portos = SnapshotV2["portos"];
type Perform = (
    work: (signal: AbortSignal) => Promise<string>,
    message?: string,
) => Promise<unknown>;

const linkLabels: Record<Portos["link_status"], string> = {
    unconfigured: "Unconfigured",
    link_required: "Link required",
    linked: "Linked",
    relink_for_scope: "Relink needed for scope",
};
const operationLabels: Record<Portos["operation_state"], string> = {
    idle: "",
    linking: "Waiting for Port OS approval.",
    unlinking: "Unlinking Port OS…",
    expired: "Port OS code expired. Link again to get a new code.",
    denied: "Port OS denied the link request.",
    failed: "Port OS could not finish the operation. Check the link status before trying again.",
};

export function mountPortos(
    parent: AccountRow,
    permissions: HTMLElement,
    actions: string,
    initial: SnapshotV2,
    perform: Perform,
    // `transient` marks a plain result; warnings and errors stay shown.
    updateRestart: (message: string, transient?: boolean) => void,
) {
    let status = initial.portos;
    let waiting = initial.restart_pending;
    let restartFailed = initial.warnings.includes(restartWarning);
    let endpointMessage = initial.warnings.find((warning) =>
        endpointMessages.has(warning),
    );
    let endpointRefresh = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let read: AbortController | undefined;
    const linkStatus = element("p");
    const relinkAdvice = element(
        "p",
        "Relink to use this Echo's own Port OS client",
    );
    const operationStatus = element("p");
    operationStatus.setAttribute("aria-live", "polite");
    const endpointStatus = element("p");
    endpointStatus.setAttribute("aria-live", "polite");
    const granted = element("p");
    const challenge = accountChallenge();
    const buttons = accountButtons(parent, "Port OS", start);
    parent.section.append(
        linkStatus,
        relinkAdvice,
        buttons.controls,
        operationStatus,
        challenge.instruction,
        challenge.code,
        endpointStatus,
    );
    permissions.prepend(granted);

    function active() {
        return (
            status.operation_state === "linking" ||
            status.operation_state === "unlinking"
        );
    }

    function render() {
        linkStatus.textContent = linkLabels[status.link_status];
        relinkAdvice.hidden = status.relink_advised !== true;
        granted.hidden = status.server === undefined;
        const scopes = status.server?.granted_scopes.join(", ");
        granted.textContent = "Granted scopes: " + (scopes || "None");
        operationStatus.textContent =
            operationLabels[status.operation_state] ||
            (status.link_waiting_for_restart
                ? restartFailed
                    ? "Bridge restart failed. Saved permissions are still pending."
                    : "Linking is available after the bridge restarts."
                : "");
        operationStatus.hidden = operationStatus.textContent === "";
        if (endpointStatus.textContent !== (endpointMessage ?? ""))
            endpointStatus.textContent = endpointMessage ?? "";
        endpointStatus.hidden = !endpointMessage;
        challenge.render(status.challenge);
        buttons.render(
            !!status.server &&
                status.link_status !== "linked" &&
                !status.link_waiting_for_restart &&
                !active(),
            (status.link_status === "linked" ||
                status.link_status === "relink_for_scope") &&
                !active(),
            status.link_status === "relink_for_scope",
        );
    }

    function stop() {
        clearTimeout(timer);
        timer = undefined;
        read?.abort();
        read = undefined;
    }

    function monitor() {
        if (timer || read) return;
        const pairing = endpointMessage === endpointConnecting;
        if (
            endpointRefresh ||
            pairing ||
            active() ||
            (waiting && !restartFailed)
        )
            timer = setTimeout(() => void poll(), 500);
    }

    async function poll() {
        timer = undefined;
        const controller = new AbortController();
        read = controller;
        const deadline = setTimeout(() => controller.abort(), 10000);
        endpointRefresh = false;
        try {
            const result = await readSettingsProgress(
                actions,
                controller.signal,
                waiting,
            );
            if (!result || controller.signal.aborted || read !== controller)
                return;
            status = result.portos;
            endpointMessage = result.warnings.find((warning) =>
                endpointMessages.has(warning),
            );
            restartFailed = result.warnings.includes(restartWarning);
            if (waiting && restartFailed) updateRestart(restartWarning);
            if (waiting && !result.restart_pending) {
                const overlayWarning = result.warnings.find((warning) =>
                    warning.startsWith("settings.toml: ignored overlay:"),
                );
                if (overlayWarning) updateRestart(overlayWarning);
                else
                    updateRestart(
                        "Bridge restarted. Saved settings are active.",
                        true,
                    );
            }
            waiting = result.restart_pending;
            render();
        } catch {
            if (
                read === controller &&
                !(waiting && controller.signal.aborted)
            ) {
                challenge.render();
                operationStatus.hidden = false;
                operationStatus.textContent =
                    "Port OS status is unavailable. Reopen Settings to check the operation.";
                if (waiting)
                    updateRestart(
                        "Restart status is unavailable. Reopen Settings to check the saved settings.",
                    );
                read = undefined;
            }
        } finally {
            clearTimeout(deadline);
            if (read === controller) {
                read = undefined;
                monitor();
            }
        }
    }

    function start(body: PortosOperationV2) {
        void perform(async (signal) => {
            stop();
            try {
                await operateAccount(actions, "portos", signal, body);
                if (signal.aborted) return "";
                status = {
                    ...status,
                    operation_state:
                        body.operation === "link" ? "linking" : "unlinking",
                };
                render();
                return "Port OS operation started.";
            } finally {
                if (!signal.aborted) monitor();
            }
        });
    }

    render();
    monitor();
    return {
        stop,
        settingsSaved(
            restarting: boolean,
            scopesChanged: boolean,
            endpointChanged: boolean,
        ) {
            stop();
            if (restarting) {
                waiting = true;
                restartFailed = false;
            }
            endpointRefresh = endpointChanged;
            if (scopesChanged)
                status = { ...status, link_waiting_for_restart: true };
            render();
            monitor();
        },
    };
}

export function portosEndpointForm(
    status: Portos,
    save: Save<PatchV2>,
    track: Track,
) {
    if (
        status.endpoint_name === undefined ||
        status.endpoint_enabled === undefined
    )
        return;
    let acceptedName = status.endpoint_name;
    let acceptedEnabled = status.endpoint_enabled;
    const form = element("form");
    const name = element("input");
    name.value = acceptedName;
    const enabled = element("input");
    enabled.type = "checkbox";
    enabled.checked = acceptedEnabled;
    const nameRow = field("Name in Port OS", name);
    const error = element("p");
    error.id = name.id + "-error";
    error.setAttribute("aria-live", "polite");
    name.setAttribute("aria-describedby", error.id);
    form.append(
        nameRow,
        error,
        field("Connect this Echo to Port OS", enabled),
        formActions(element("button", "Save Port OS endpoint")),
    );

    async function submit() {
        const endpointName = name.value;
        const endpointEnabled = enabled.checked;
        if (!validEndpointName(endpointName)) {
            name.setAttribute("aria-invalid", "true");
            error.textContent =
                "Enter 1 to 64 characters without control characters.";
            name.focus();
            return false;
        }
        name.removeAttribute("aria-invalid");
        error.textContent = "";
        const patch: NonNullable<PatchV2["portos"]> = {};
        if (endpointName !== acceptedName) patch.endpoint_name = endpointName;
        if (endpointEnabled !== acceptedEnabled)
            patch.endpoint_enabled = endpointEnabled;
        const saved = await save(
            Object.keys(patch).length ? { portos: patch } : {},
        );
        if (saved) {
            acceptedName = endpointName;
            acceptedEnabled = endpointEnabled;
        }
        return saved;
    }

    form.addEventListener("submit", (event) => {
        event.preventDefault();
        void submit();
    });
    track({
        dirty: () =>
            name.value !== acceptedName || enabled.checked !== acceptedEnabled,
        save: submit,
    });
    return form;
}
