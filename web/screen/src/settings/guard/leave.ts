import { actions, element } from "../forms";

type Choices = { save: () => void; discard: () => void; cancel: () => void };

// The question Settings asks, in the page, before it closes over unsaved
// edits. It hides itself before it reports the answer.
export function leavePrompt(choices: Choices) {
    const overlay = element("div");
    overlay.className = "settings-leave";
    overlay.hidden = true;
    const dialog = element("div");
    dialog.setAttribute("role", "alertdialog");
    dialog.setAttribute("aria-modal", "true");
    const question = element("p", "Save your changes before leaving?");
    question.id = "settings-leave-question";
    dialog.setAttribute("aria-labelledby", question.id);
    const buttons = ["Cancel", "Don't save", "Save"].map((text) => {
        const button = element("button", text);
        button.type = "button";
        return button;
    });
    const [cancel, discard, save] = buttons;
    dialog.append(question, actions(...buttons));
    overlay.append(dialog);
    function answer(choice: () => void) {
        overlay.hidden = true;
        choice();
    }
    cancel?.addEventListener("click", () => answer(choices.cancel));
    discard?.addEventListener("click", () => answer(choices.discard));
    save?.addEventListener("click", () => answer(choices.save));
    // A tap outside the box is Cancel.
    overlay.addEventListener("click", (event) => {
        if (event.target === overlay) answer(choices.cancel);
    });
    overlay.addEventListener("keydown", (event) => {
        // The panel's own Escape would ask the question again.
        event.stopPropagation();
        if (event.key === "Escape") answer(choices.cancel);
        else if (event.key === "Tab") {
            // Focus stays on the three answers while the question is open.
            event.preventDefault();
            const at = buttons.indexOf(event.target as HTMLButtonElement);
            buttons[(at + (event.shiftKey ? 2 : 1)) % 3]?.focus();
        } else if (
            event.key === "Enter" &&
            event.target !== cancel &&
            event.target !== discard
        ) {
            // Enter means Save; it must not also press the hidden button.
            event.preventDefault();
            answer(choices.save);
        }
    });
    return {
        element: overlay,
        get open() {
            return !overlay.hidden;
        },
        ask() {
            overlay.hidden = false;
            save?.focus();
        },
    };
}
