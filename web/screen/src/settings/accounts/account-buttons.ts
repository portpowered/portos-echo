import { element } from "../forms";
import type { PortosOperationV2 } from "../model";

export function accountButtons(
    name: string,
    start: (operation: PortosOperationV2) => void,
) {
    const makeButton = (text: string) => {
        const button = element("button", text);
        button.type = "button";
        return button;
    };
    const link = makeButton("Link " + name);
    const unlink = makeButton("Unlink " + name);
    const cancel = makeButton("Cancel");
    const confirm = makeButton("Confirm unlink");
    const confirmation = element("div");
    confirmation.hidden = true;
    confirmation.append(element("p", "Unlink " + name + "?"), cancel, confirm);
    const controls = element("div");
    controls.append(link, unlink, confirmation);
    let canLink = false;
    let canUnlink = false;
    function render() {
        link.hidden = !canLink || !confirmation.hidden;
        unlink.hidden = !canUnlink || !confirmation.hidden;
    }
    function dismiss() {
        confirmation.hidden = true;
        render();
        unlink.focus();
    }
    link.addEventListener("click", () =>
        start({ version: 2, operation: "link" }),
    );
    unlink.addEventListener("click", () => {
        confirmation.hidden = false;
        render();
        cancel.focus();
    });
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

    return {
        controls,
        render(linkAllowed: boolean, unlinkAllowed: boolean, relink = false) {
            canLink = linkAllowed;
            canUnlink = unlinkAllowed;
            link.textContent = (relink ? "Relink " : "Link ") + name;
            if (!canUnlink && !confirmation.hidden) dismiss();
            render();
        },
    };
}

export function accountChallenge() {
    const instruction = element("p");
    const code = element("p");
    return {
        instruction,
        code,
        render(challenge?: {
            verification_uri: string;
            user_code: string;
            expires_at_unix: number;
        }) {
            instruction.hidden = code.hidden = !challenge;
            instruction.textContent = challenge
                ? "On a phone or PC, open " + challenge.verification_uri
                : "";
            code.textContent = challenge
                ? "Code: " +
                  challenge.user_code +
                  " · Expires at " +
                  new Date(
                      challenge.expires_at_unix * 1000,
                  ).toLocaleTimeString()
                : "";
        },
    };
}
