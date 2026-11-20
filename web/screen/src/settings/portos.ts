import { loadPortos, operatePortos } from "./api-v2";
import type { SnapshotV2 } from "./api";
import { element } from "./forms";
import type { PortosOperationV2 } from "./model";

type Portos = SnapshotV2["portos"];
type Perform = (
    work: (signal: AbortSignal) => Promise<string>,
    message?: string,
) => Promise<void>;

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

function button(label: string) {
    const node = element("button", label);
    node.type = "button";
    return node;
}

export function mountPortos(
    parent: HTMLElement,
    actions: string,
    initial: Portos,
    perform: Perform,
    restartFailed: boolean,
) {
    let status = initial;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let read: AbortController | undefined;
    const linkStatus = element("p");
    const operationStatus = element("p");
    operationStatus.setAttribute("aria-live", "polite");
    const granted = element("p");
    const instruction = element("p");
    const code = element("p");
    const link = button("Link Port OS");
    const unlink = button("Unlink Port OS");
    const confirmation = element("div");
    const cancel = button("Cancel");
    const confirm = button("Confirm unlink");
    confirmation.hidden = true;
    confirmation.append(element("p", "Unlink Port OS?"), cancel, confirm);
    parent.append(
        linkStatus,
        granted,
        operationStatus,
        instruction,
        code,
        link,
        unlink,
        confirmation,
    );

    function active() {
        return (
            status.operation_state === "linking" ||
            status.operation_state === "unlinking"
        );
    }

    function render() {
        linkStatus.textContent = linkLabels[status.link_status];
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
        const challenge = status.challenge;
        instruction.hidden = code.hidden = !challenge;
        instruction.textContent = challenge
            ? "On a phone or PC, open " + challenge.verification_uri
            : "";
        code.textContent = challenge
            ? "Code: " +
              challenge.user_code +
              " · Expires at " +
              new Date(challenge.expires_at_unix * 1000).toLocaleTimeString()
            : "";
        link.hidden =
            !status.server ||
            status.link_status === "linked" ||
            status.link_waiting_for_restart ||
            active() ||
            !confirmation.hidden;
        link.textContent =
            status.link_status === "relink_for_scope"
                ? "Relink Port OS"
                : "Link Port OS";
        unlink.hidden =
            (status.link_status !== "linked" &&
                status.link_status !== "relink_for_scope") ||
            active() ||
            !confirmation.hidden;
    }

    function stop() {
        clearTimeout(timer);
        timer = undefined;
        read?.abort();
        read = undefined;
    }

    function monitor() {
        if (timer || read) return;
        if (active() || (status.link_waiting_for_restart && !restartFailed))
            timer = setTimeout(() => void poll(), 500);
    }

    async function poll() {
        timer = undefined;
        const controller = new AbortController();
        read = controller;
        const deadline = setTimeout(() => controller.abort(), 10000);
        try {
            const result = await loadPortos(actions, controller.signal);
            if (controller.signal.aborted || read !== controller) return;
            status = result.status;
            restartFailed = result.restartFailed;
            render();
        } catch {
            if (read === controller) {
                instruction.hidden = code.hidden = true;
                operationStatus.hidden = false;
                operationStatus.textContent =
                    "Port OS status is unavailable. Reopen Settings to check the operation.";
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
                await operatePortos(actions, signal, body);
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

    link.addEventListener("click", () =>
        start({ version: 2, operation: "link" }),
    );
    unlink.addEventListener("click", () => {
        confirmation.hidden = false;
        render();
        cancel.focus();
    });
    function dismiss() {
        confirmation.hidden = true;
        render();
        unlink.focus();
    }
    cancel.addEventListener("click", dismiss);
    confirmation.addEventListener("keydown", (event) => {
        if (event.key === "Escape") {
            event.stopPropagation();
            dismiss();
        }
    });
    confirm.addEventListener("click", () => {
        dismiss();
        start({ version: 2, operation: "unlink", confirm: true });
    });

    render();
    monitor();
    return {
        stop,
        scopesSaved() {
            status = { ...status, link_waiting_for_restart: true };
            render();
            monitor();
        },
    };
}
