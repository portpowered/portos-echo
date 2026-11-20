import { formatExpiry } from "../../expiry";
import { element } from "../forms";
import type { PortosOperationV2 } from "../model";

const makeButton = (text: string) => {
    const button = element("button", text);
    button.type = "button";
    return button;
};

export type AccountRow = { section: HTMLElement; header: HTMLElement };

// A deletable item: its title at the top left, and its action (Link or
// Remove, added by the owner of the item) on the same line, at the top right.
export function accountRow(title: string): AccountRow {
    const section = element("section");
    section.setAttribute("aria-label", title);
    const header = element("div");
    header.className = "settings-item-header";
    header.append(element("h3", title));
    section.append(header);
    return { section, header };
}

// Every Remove control in Settings: the same label, and the place and size of
// its item header's action; the accessible name says what it removes.
export function removeButton(what: string) {
    const button = makeButton("Remove");
    button.setAttribute("aria-label", "Remove " + what);
    return button;
}

export function accountButtons(
    row: AccountRow,
    name: string,
    start: (operation: PortosOperationV2) => void,
) {
    const link = makeButton("Link " + name);
    const unlink = removeButton(name + " link");
    const cancel = makeButton("Cancel");
    const confirm = makeButton("Remove link");
    const confirmation = element("div");
    confirmation.hidden = true;
    confirmation.className = "settings-account-confirm";
    confirmation.append(
        element("p", "Remove the " + name + " link?"),
        cancel,
        confirm,
    );
    // Link and Remove are the row's action, beside its title; the Remove
    // confirmation opens below the status line.
    row.header.append(link, unlink);
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
        controls: confirmation,
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
                  " · " +
                  formatExpiry(challenge.expires_at_unix, "local-time").label
                : "";
        },
    };
}
