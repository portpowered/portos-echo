import { operateAccount, readOpenAI } from "../api-v2";
import { element } from "../forms";
import type { OpenAiAccountV2, PortosOperationV2 } from "../model";
import { apiKeyStatus, openAIKeyForm } from "./openai-key";
import {
    accountButtons,
    accountChallenge,
    type AccountRow,
} from "./account-buttons";

const linkLabels: Record<OpenAiAccountV2["link_status"], string> = {
    checking: "Checking OpenAI account…",
    unavailable: "OpenAI sign-in unavailable",
    link_required: "Not linked",
    linked: "Linked",
};
const operationLabels: Record<OpenAiAccountV2["operation_state"], string> = {
    idle: "",
    linking: "Waiting for OpenAI approval.",
    unlinking: "Unlinking OpenAI after the current conversation settles…",
    expired: "OpenAI sign-in expired. Link again to get a new code.",
    denied: "OpenAI declined the link request.",
    failed: "OpenAI could not finish the operation. Check the account status before trying again.",
};

export function mountOpenAI(
    parent: AccountRow,
    actions: string,
    perform: (
        work: (signal: AbortSignal) => Promise<string>,
    ) => Promise<unknown>,
) {
    const linkStatus = element("p");
    const operationStatus = element("p");
    operationStatus.setAttribute("aria-live", "polite");
    const challenge = accountChallenge();
    const buttons = accountButtons(parent, "OpenAI", start);
    const keyForm = openAIKeyForm(actions, perform, () => void refresh());
    parent.section.append(
        linkStatus,
        buttons.controls,
        keyForm.form,
        operationStatus,
        challenge.instruction,
        challenge.code,
    );
    let status: OpenAiAccountV2 | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let current: AbortController | undefined;

    function active() {
        return (
            status?.operation_state === "linking" ||
            status?.operation_state === "unlinking"
        );
    }

    function render() {
        linkStatus.textContent =
            (status && apiKeyStatus(status)) ??
            linkLabels[status?.link_status ?? "unavailable"];
        keyForm.render(status);
        const signIn = !status?.api_key;
        operationStatus.textContent =
            operationLabels[status?.operation_state ?? "idle"];
        operationStatus.hidden = !operationStatus.textContent;
        challenge.render(status?.challenge);
        buttons.render(
            signIn && !active() && status?.link_status === "link_required",
            signIn && !active() && status?.link_status === "linked",
        );
    }

    function stop() {
        clearTimeout(timer);
        current?.abort();
        current = undefined;
    }

    async function refresh() {
        stop();
        const controller = new AbortController();
        current = controller;
        const timeout = setTimeout(() => controller.abort(), 10_000);
        try {
            const account = await readOpenAI(actions, controller.signal);
            if (current !== controller) return;
            status = account;
            render();
            if (active() || status?.link_status === "checking")
                timer = setTimeout(() => void refresh(), 500);
        } catch {
            if (current !== controller) return;
            status = undefined;
            render();
            linkStatus.textContent =
                "OpenAI status is unavailable. Reopen Settings to check the account.";
        } finally {
            clearTimeout(timeout);
            if (current === controller) current = undefined;
        }
    }

    function start(body: PortosOperationV2) {
        void perform(async (signal) => {
            stop();
            try {
                await operateAccount(actions, "openai", signal, body);
                return "OpenAI operation started.";
            } finally {
                if (!signal.aborted) void refresh();
            }
        });
    }

    linkStatus.textContent = linkLabels.checking;
    operationStatus.hidden = true;
    challenge.render();
    void refresh();
    return { stop, refresh };
}
