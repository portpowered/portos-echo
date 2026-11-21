import type { ScreenFrame, UiAction, UiState } from "./model";
import { Presentation } from "./state";
import { endpoints, postAction } from "./transport";
import { animate } from "./emotion";
import { formatExpiry } from "./expiry";
import { mountSettings } from "./settings/view";
import { mountPacks } from "./packs/view";

function text(tag: string, value: string): HTMLElement {
    const node = document.createElement(tag);
    node.textContent = value;
    return node;
}

export function startPage(root: HTMLElement, href: string): () => void {
    const content = document.createElement("section");
    content.className = "screen-presentation";
    content.setAttribute("aria-label", "Screen presentation");
    const diagnostic = text("p", "");
    diagnostic.setAttribute("role", "status");
    const clock = document.createElement("div");
    clock.className = "home-clock";
    const time = text("p", "");
    const date = text("p", "");
    clock.append(time, date);
    root.replaceChildren(content, diagnostic);
    const presentation = new Presentation();
    let stream: EventSource;
    let urls: ReturnType<typeof endpoints>;
    let pending: AbortController | undefined;
    let disposed = false;
    let cancelPending: (() => void) | undefined;
    let stopAnimation: (() => void) | undefined;
    let clockTimer: ReturnType<typeof setTimeout> | undefined;
    let expiryTimer: ReturnType<typeof setTimeout> | undefined;
    let settings: ReturnType<typeof mountSettings> | undefined;
    let packs: ReturnType<typeof mountPacks> | undefined;
    async function send(action: UiAction, frame: ScreenFrame): Promise<void> {
        if (disposed || pending || presentation.current !== frame) return;
        const controller = new AbortController();
        const focused = document.activeElement;
        pending = controller;
        for (const button of content.querySelectorAll("button"))
            button.disabled = true;
        diagnostic.textContent = "Sending action…";
        let timer: ReturnType<typeof setTimeout>;
        const failure = new Promise<never>((_, reject) => {
            cancelPending = () => {
                controller.abort();
                reject(new Error("Action cancelled"));
            };
            timer = setTimeout(() => cancelPending?.(), 10000);
        });
        try {
            await Promise.race([
                postAction(urls.actions, action, controller.signal),
                failure,
            ]);
            if (!disposed && presentation.current === frame)
                diagnostic.textContent =
                    "Action sent; waiting for screen update";
        } catch {
            if (!disposed && presentation.current === frame)
                diagnostic.textContent = "Action could not be sent";
        } finally {
            clearTimeout(timer!);
            pending = undefined;
            cancelPending = undefined;
            if (!disposed)
                for (const button of content.querySelectorAll("button"))
                    button.disabled = false;
            if (
                !disposed &&
                presentation.current === frame &&
                focused instanceof HTMLButtonElement &&
                focused.isConnected
            )
                focused.focus();
        }
    }
    function button(
        label: string,
        action: UiAction,
        frame: ScreenFrame,
    ): HTMLButtonElement {
        const node = document.createElement("button");
        node.textContent = label;
        node.disabled = !!pending;
        // Native click handles keyboard, and detached controls cannot finish stale gestures.
        node.addEventListener("click", () => {
            void send(action, frame);
        });
        return node;
    }
    function refreshClock(): void {
        const now = new Date();
        time.textContent = now.toLocaleTimeString([], {
            hour: "2-digit",
            minute: "2-digit",
        });
        date.textContent = now.toLocaleDateString();
        clockTimer = setTimeout(refreshClock, 60000 - (now.getTime() % 60000));
    }
    function renderLinkCode(
        details: HTMLElement,
        state: Extract<UiState, { state: "link_code" }>,
    ): void {
        const expiry = text("p", "");
        details.append(
            text("p", state.user_code),
            text("p", state.verification_uri),
            expiry,
        );
        function refresh(): void {
            const { label, refreshInMs } = formatExpiry(
                state.expires_at_unix,
                "countdown",
            );
            expiry.textContent = label;
            expiryTimer =
                refreshInMs === undefined
                    ? undefined
                    : setTimeout(refresh, refreshInMs);
        }
        refresh();
    }
    function render(frame: ScreenFrame): void {
        settings?.update(frame.state);
        stopAnimation?.();
        cancelPending?.();
        clearTimeout(clockTimer);
        clearTimeout(expiryTimer);
        expiryTimer = undefined;
        const state = frame.state;
        const idle = state.state === "ready" || state.state === "connected";
        // A pack's own nodes stay where they are: moving its stylesheet link
        // would load the sheet again and restyle the clock for a moment.
        for (const node of [...content.childNodes])
            if (!(
                node instanceof HTMLElement && "packPersistent" in node.dataset
            ))
                node.remove();
        content.dataset.uiState = state.state;
        // A pack's face shows the state itself; only prompts keep a heading.
        const face =
            !!content.querySelector(":scope > .pack-stage") &&
            !["link_code", "approval", "error"].includes(state.state);
        const heading = text("h1", state.state.replace(/_/g, " "));
        if (!idle && !face) content.append(heading);
        const details = document.createElement("div");
        details.className = "details";
        const controls = document.createElement("div");
        controls.className = "controls";
        content.append(details, controls);
        content.dataset.seq = frame.seq.toString();
        diagnostic.textContent = "";
        if (idle && !content.classList.contains("screen-pack")) {
            content.append(clock);
            refreshClock();
        }
        switch (state.state) {
            case "ready": {
                const tap = button(
                    "",
                    { action: "tap", target: "primary" },
                    frame,
                );
                tap.className = "home-tap";
                tap.setAttribute("aria-label", "Begin session");
                content.append(tap);
                break;
            }
            case "link_code":
                renderLinkCode(details, state);
                break;
            case "approval":
                details.append(text("p", state.prompt));
                controls.append(
                    button(
                        state.accept_label,
                        { action: "approve", id: state.id },
                        frame,
                    ),
                    button(
                        state.deny_label,
                        { action: "deny", id: state.id },
                        frame,
                    ),
                );
                break;
            case "error":
                details.append(text("p", state.message));
                if (state.relink)
                    controls.append(
                        button("Relink", { action: "relink" }, frame),
                    );
                controls.append(
                    button("Dismiss", { action: "dismiss" }, frame),
                );
                break;
            case "emotion":
                details.append(text("p", state.name));
                {
                    const animation = text("p", "");
                    animation.setAttribute("aria-label", "Animation frame");
                    details.append(animation);
                    stopAnimation = animate(
                        state.frames,
                        state.frame_ms,
                        (value) => {
                            animation.textContent = value;
                        },
                    );
                }
                controls.append(
                    button("Dismiss", { action: "dismiss" }, frame),
                );
                break;
        }
        if (packs?.update(state)) {
            stopAnimation?.();
            details.replaceChildren();
            // The face plays the emote; a tap anywhere dismisses it, with no box.
            const dismiss = controls.querySelector("button");
            if (state.state === "emotion" && dismiss) {
                dismiss.textContent = "";
                dismiss.className = "home-tap";
                dismiss.setAttribute("aria-label", "Dismiss");
                content.append(dismiss);
            }
        } else if (state.state === "emotion" && face) {
            // An emote the pack lacks keeps the native frame animation.
            content.insertBefore(heading, details);
        }
    }
    try {
        urls = endpoints(href);
        stream = new EventSource(urls.events);
        if (document.querySelector('meta[name="echo-packs"][content="v1"]'))
            packs = mountPacks(content, urls.actions, stream, () => {
                if (presentation.current) render(presentation.current);
            });
        if (document.querySelector('meta[name="echo-settings"][content="v1"]'))
            settings = mountSettings(
                root,
                content,
                diagnostic,
                urls.actions,
                (signal) =>
                    postAction(urls.actions, { action: "relink" }, signal),
            );
    } catch {
        diagnostic.textContent = "Screen configuration is invalid";
        return () => {};
    }
    stream.onmessage = (event) => {
        if (disposed) return;
        try {
            if (presentation.receive(event.data) && presentation.current)
                render(presentation.current);
        } catch {
            diagnostic.textContent = "Screen update could not be read";
        }
    };
    stream.onerror = () => {
        if (disposed) return;
        diagnostic.textContent = "Screen connection interrupted";
    };
    function dispose(): void {
        if (disposed) return;
        disposed = true;
        stopAnimation?.();
        clearTimeout(clockTimer);
        clearTimeout(expiryTimer);
        settings?.dispose();
        packs?.dispose();
        stream.close();
        cancelPending?.();
        window.removeEventListener("pagehide", dispose);
    }
    window.addEventListener("pagehide", dispose);
    return dispose;
}
