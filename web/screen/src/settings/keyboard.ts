import { holdRepeat } from "./stepper";

// A small on-screen keyboard for the settings panel: the Echo has no input
// method, so focusing a field shows nothing. It never sets inputmode, so a real
// keyboard keeps working if one is ever installed.
type Field = HTMLInputElement | HTMLTextAreaElement;
type Layer = "letters" | "symbols";

// Each layer has a plain and a shifted page of three rows.
const ROWS: Record<Layer, [string[], string[]]> = {
    letters: [
        [
            "q w e r t y u i o p",
            "a s d f g h j k l back",
            "shift z x c v b n m , .",
        ],
        [
            "Q W E R T Y U I O P",
            "A S D F G H J K L back",
            "shift Z X C V B N M , .",
        ],
    ],
    symbols: [
        [
            "1 2 3 4 5 6 7 8 9 0",
            '- / : ; ( ) & @ " back',
            "shift ! ? ' # * + = _ `",
        ],
        [
            "[ ] { } < > ^ ~ | \\",
            "% $ € £ ¥ ° • … — back",
            "shift ¡ ¿ « » § ± × ÷ ·",
        ],
    ],
};
const NAMED: Record<string, [label: string, name: string]> = {
    back: ["Del", "Backspace"],
    shift: ["Shift", "Shift"],
    space: ["space", "Space"],
    enter: ["Enter", "Enter"],
    done: ["Done", "Done"],
};

function editable(node: unknown): node is Field {
    return (
        node instanceof HTMLTextAreaElement ||
        (node instanceof HTMLInputElement &&
            (node.type === "text" || node.type === "password"))
    );
}

export function mountKeyboard(panel: HTMLElement, fields: HTMLElement) {
    const board = document.createElement("div");
    board.className = "settings-keyboard";
    board.hidden = true;
    board.setAttribute("role", "group");
    board.setAttribute("aria-label", "On-screen keyboard");
    panel.append(board);
    let target: Field | undefined;
    let layer: Layer = "letters";
    let shift = false;

    function close() {
        target = undefined;
        board.hidden = true;
    }
    function changed(field: Field, inputType: string, data: string | null) {
        field.dispatchEvent(
            new InputEvent("input", { bubbles: true, inputType, data }),
        );
        if (field.selectionEnd === field.value.length)
            field.scrollTop = field.scrollHeight;
        if (document.activeElement !== field) field.focus();
    }
    function usable(): Field | undefined {
        return target?.isConnected && !target.disabled && !target.readOnly
            ? target
            : undefined;
    }
    function insert(text: string) {
        const field = usable();
        if (!field) return;
        const start = field.selectionStart ?? field.value.length;
        field.setRangeText(text, start, field.selectionEnd ?? start, "end");
        changed(field, "insertText", text);
    }
    function erase() {
        const field = usable();
        if (!field) return;
        const end = field.selectionEnd ?? field.value.length;
        let start = field.selectionStart ?? end;
        // An empty selection removes one whole character before the caret.
        if (start === end)
            start -= (Array.from(field.value.slice(0, start)).pop() ?? "")
                .length;
        if (start === end) return;
        field.setRangeText("", start, end, "end");
        changed(field, "deleteContentBackward", null);
    }
    function key(token: string): HTMLButtonElement {
        const node = document.createElement("button");
        node.type = "button";
        node.tabIndex = -1;
        node.dataset.key = token;
        const named: [string, string] | undefined =
            token === "layer"
                ? layer === "letters"
                    ? ["123", "Symbols"]
                    : ["abc", "Letters"]
                : NAMED[token];
        node.textContent = named ? named[0] : token;
        if (named) {
            node.setAttribute("aria-label", named[1]);
            node.className = token === "space" ? "key-space" : "key-named";
        }
        if (token === "shift") node.setAttribute("aria-pressed", `${shift}`);
        if (token === "back") holdRepeat(node, erase);
        return node;
    }
    function draw() {
        board.replaceChildren(
            ...[...ROWS[layer][shift ? 1 : 0], "layer space enter done"].map(
                (row) => {
                    const line = document.createElement("div");
                    line.append(...row.split(" ").map(key));
                    return line;
                },
            ),
        );
    }
    function open(field: Field) {
        if (target !== field) {
            layer = field.inputMode === "numeric" ? "symbols" : "letters";
            shift = false;
            draw();
        }
        target = field;
        board.hidden = false;
        // The keyboard takes its height from the fields area, never overlays it.
        field.scrollIntoView({ block: "nearest" });
    }
    function press(token: string) {
        if (token === "done") close();
        else if (token === "shift") shift = !shift;
        else if (token === "layer") {
            layer = layer === "letters" ? "symbols" : "letters";
            shift = false;
        } else if (token === "enter") {
            if (target instanceof HTMLTextAreaElement) insert("\n");
            else close();
        } else {
            insert(token === "space" ? " " : token);
            // Shift is one-shot for letters and a second page for symbols.
            if (layer !== "letters" || !shift) return;
            shift = false;
        }
        draw();
    }
    // Keys must not take focus (or the caret) away from the field being typed in.
    for (const name of ["pointerdown", "mousedown"])
        board.addEventListener(name, (event) => event.preventDefault());
    board.addEventListener("click", (event) => {
        // Redrawing detaches the pressed key, so the panel must not see this.
        event.stopPropagation();
        const token =
            event.target instanceof Element
                ? event.target.closest("button")?.dataset.key
                : undefined;
        if (token && token !== "back") press(token);
    });
    // Opening moves the fields under the finger, so the click that ends the
    // same tap can land on another element; `fresh` makes that click harmless.
    let fresh = false;
    panel.addEventListener("pointerdown", () => (fresh = false), true);
    fields.addEventListener("focusin", (event) => {
        if (!editable(event.target)) return;
        open(event.target);
        fresh = true;
    });
    // A tap on a text field reopens after Done; a tap elsewhere puts it away.
    // Closing on click, not on blur, keeps the tapped control under the finger.
    panel.addEventListener("click", (event) => {
        if (fresh) fresh = false;
        else if (editable(event.target) && fields.contains(event.target))
            open(event.target);
        else close();
    });
    return { close };
}
