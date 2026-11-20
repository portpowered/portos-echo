// The Echo has no system keyboard, so numbers are changed with − / + buttons.
const HOLD_MS = 500;
const REPEAT_MS = 100;
const FAST_AFTER = 10;

// A tap presses once (repeat 0). Holding presses again every REPEAT_MS after
// HOLD_MS, with repeat counting from 1, until the pointer lifts or leaves.
export function holdRepeat(
    button: HTMLButtonElement,
    press: (repeat: number) => void,
): void {
    let timer: ReturnType<typeof setTimeout> | undefined;
    let repeats = 0;
    const stop = () => {
        clearTimeout(timer);
        timer = undefined;
    };
    const tick = () => {
        timer = undefined;
        if (button.disabled || !button.getClientRects().length) return;
        press(++repeats);
        timer = setTimeout(tick, REPEAT_MS);
    };
    button.addEventListener("pointerdown", () => {
        stop();
        repeats = 0;
        timer = setTimeout(tick, HOLD_MS);
    });
    for (const name of ["pointerup", "pointercancel", "pointerleave"])
        button.addEventListener(name, stop);
    button.addEventListener("contextmenu", (event) => event.preventDefault());
    button.addEventListener("click", () => {
        stop();
        // The click that ends a hold must not add one more press.
        if (repeats) repeats = 0;
        else press(0);
    });
}

// Wraps a whole-number input with − / + buttons clamped to [min, max]. A long
// hold moves by `fast` per press. BigInt keeps the 19-digit daily cap exact.
export function stepper(
    input: HTMLInputElement,
    min: bigint,
    max: bigint,
    fast = 1n,
): HTMLDivElement {
    const box = document.createElement("div");
    box.className = "settings-stepper";
    const button = (label: string, name: string, sign: bigint) => {
        const node = document.createElement("button");
        node.type = "button";
        node.textContent = label;
        node.setAttribute("aria-label", name);
        holdRepeat(node, (repeat) => {
            const stride = repeat > FAST_AFTER ? fast : 1n;
            let next = /^[0-9]+$/.test(input.value)
                ? BigInt(input.value) + sign * stride
                : min;
            if (next < min) next = min;
            if (next > max) next = max;
            input.value = next.toString();
            input.dispatchEvent(new Event("input", { bubbles: true }));
        });
        return node;
    };
    box.append(
        button("−", "Decrease", -1n),
        input,
        button("+", "Increase", 1n),
    );
    return box;
}
