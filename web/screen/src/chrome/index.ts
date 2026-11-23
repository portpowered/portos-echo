import type { UiState } from "../model";

const ATTRIBUTE = "data-echo-screen-chrome";
const WAITING = "Waiting for bridge";

export function mountChrome(onMode: (enabled: boolean) => void) {
    const html = document.documentElement;
    const grip = document.createElement("div");
    const band = document.createElement("div");
    const status = document.createElement("p");
    grip.className = "screen-grip";
    grip.setAttribute("aria-hidden", "true");
    band.setAttribute("aria-hidden", "true");
    status.className = "screen-bridge-status";
    status.setAttribute("role", "status");
    let enabled = false;
    let state: UiState["state"] | undefined;
    let waiting = true;

    function paint(): void {
        const pattern = waiting ? undefined : state;
        switch (pattern) {
            case "listening":
            case "thinking":
            case "speaking":
            case "muted":
            case "error":
                band.className = `screen-band band-${pattern}`;
                break;
            default:
                band.className = "screen-band";
        }
        const message = waiting ? WAITING : "";
        if (status.textContent !== message) status.textContent = message;
    }

    function sync(): void {
        const next = html.getAttribute(ATTRIBUTE) === "page";
        if (next === enabled) return;
        enabled = next;
        if (enabled) {
            document.body.append(grip, band, status);
            paint();
        } else {
            grip.remove();
            band.remove();
            status.remove();
        }
        onMode(enabled);
    }

    const observer = new MutationObserver(sync);
    observer.observe(html, { attributeFilter: [ATTRIBUTE] });
    sync();
    return {
        get enabled() {
            return enabled;
        },
        update(next: UiState["state"]) {
            state = next;
            waiting = false;
            if (enabled) paint();
        },
        wait() {
            waiting = true;
            if (enabled) paint();
        },
        dispose() {
            observer.disconnect();
            grip.remove();
            band.remove();
            status.remove();
        },
    };
}
