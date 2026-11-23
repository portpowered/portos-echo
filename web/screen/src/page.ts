import type { ScreenFrame, UiAction, UiState } from "./model";
import { Presentation } from "./state";
import { endpoints, Events, postAction, watchBuild } from "./transport";
import { animate } from "./emotion";
import { formatExpiry } from "./expiry";
import { mountSettings } from "./settings/view";
import { mountPacks } from "./packs/view";
import { mouthEvents } from "./packs/mouth";
import { mountChrome } from "./chrome";

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
    let stream: Events;
    let urls: ReturnType<typeof endpoints>;
    let pending: AbortController | undefined;
    let disposed = false;
    let cancelPending: (() => void) | undefined;
    let stopAnimation: (() => void) | undefined;
    let clockTimer: ReturnType<typeof setTimeout> | undefined;
    let expiryTimer: ReturnType<typeof setTimeout> | undefined;
    let settings: ReturnType<typeof mountSettings> | undefined;
    let packs: ReturnType<typeof mountPacks> | undefined;
    let failureTimer: ReturnType<typeof setTimeout> | undefined;
    const FAILURE = "Action could not be sent";
    // Only a failed action reports, and the report clears itself: the ready
    // state emits no newer frame that would clear it.
    function clearFailure(): void {
        clearTimeout(failureTimer);
        failureTimer = undefined;
        if (diagnostic.textContent === FAILURE) diagnostic.textContent = "";
    }
    async function send(action: UiAction, frame: ScreenFrame): Promise<void> {
        if (disposed || pending || presentation.current !== frame) return;
        const controller = new AbortController();
        const focused = document.activeElement;
        pending = controller;
        for (const button of content.querySelectorAll("button"))
            button.disabled = true;
        clearFailure();
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
        } catch {
            if (!disposed && presentation.current === frame) {
                diagnostic.textContent = FAILURE;
                failureTimer = setTimeout(clearFailure, 4000);
            }
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
        state: Extract<UiState, { state: "link_code" | "open_ai_signin" }>,
        onExpired?: () => void,
    ): void {
        const expiry = text("p", "");
        const code = text("p", state.user_code);
        const uri = text("p", state.verification_uri);
        if (state.state === "open_ai_signin") code.className = "openai-code";
        details.append(code, uri, expiry);
        function refresh(): void {
            const { label, refreshInMs } = formatExpiry(
                state.expires_at_unix,
                "countdown",
            );
            expiry.textContent = label;
            if (label === "Expired" && onExpired) {
                code.remove();
                uri.remove();
                onExpired();
            }
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
        const recovery =
            state.state === "open_ai_relink" ||
            state.state === "open_ai_signin";
        const previous = content.dataset.uiState;
        const leftRecovery =
            !recovery &&
            (previous === "open_ai_relink" || previous === "open_ai_signin") &&
            content.contains(document.activeElement);
        const idle = state.state === "ready" || state.state === "connected";
        const retained = content.querySelectorAll(
            ":scope > [data-pack-persistent]",
        );
        content.replaceChildren(...retained);
        content.dataset.uiState = state.state;
        // A pack's face shows the state itself; only prompts keep a heading.
        const face =
            !!content.querySelector(":scope > .pack-stage") &&
            !recovery &&
            !["link_code", "approval", "error"].includes(state.state);
        const heading = text(
            "h1",
            recovery ? "ChatGPT sign-in" : state.state.replace(/_/g, " "),
        );
        if (!idle && !face) content.append(heading);
        const details = document.createElement("div");
        details.className = "details";
        const controls = document.createElement("div");
        controls.className = "controls";
        content.append(details, controls);
        content.dataset.seq = frame.seq.toString();
        clearFailure();
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
            case "open_ai_signin": {
                if (state.muted) details.append(text("p", "Microphone muted"));
                const attempt_id = state.attempt_id;
                controls.append(
                    button(
                        "Cancel",
                        { action: "cancel_open_ai_signin", attempt_id },
                        frame,
                    ),
                    button(
                        "Get a new code",
                        { action: "renew_open_ai_signin", attempt_id },
                        frame,
                    ),
                );
                renderLinkCode(details, state, () =>
                    controls.replaceChildren(),
                );
                break;
            }
            case "open_ai_relink": {
                if (state.muted) details.append(text("p", "Microphone muted"));
                const status = text("p", state.message);
                status.setAttribute("aria-live", "polite");
                details.append(status);
                if (!state.starting) {
                    const relink = button(
                        "Relink",
                        { action: "relink" },
                        frame,
                    );
                    controls.append(relink);
                    if (previous === "open_ai_signin" && !content.hidden)
                        relink.focus();
                }
                break;
            }
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
        if (packs?.update(state) && !recovery) {
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
        if (recovery && previous !== state.state && !content.hidden) {
            heading.tabIndex = -1;
            if (!controls.contains(document.activeElement)) heading.focus();
        } else if (leftRecovery && !content.hidden) {
            const target = content.querySelector("button") ?? content;
            if (target === content) content.tabIndex = -1;
            target.focus();
        }
    }
    try {
        urls = endpoints(href);
        const packed = !!document.querySelector(
            'meta[name="echo-packs"][content="v1"]',
        );
        // Only a page that can draw a pack's mouth asks for lip sync.
        stream = new Events(packed ? mouthEvents(urls.events) : urls.events);
        if (packed)
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
    const build = watchBuild(
        stream,
        href,
        () => settings?.dirty() ?? false,
        () => {
            offline();
            stream.restart();
        },
    );
    const chrome = mountChrome((enabled) => {
        build.setLive(enabled && !document.hidden);
    });
    function offline(): void {
        chrome.wait();
        if (presentation.current?.state.state === "open_ai_signin") {
            presentation.clear();
            cancelPending?.();
            clearTimeout(expiryTimer);
            content.querySelector(".details")?.replaceChildren();
            content.querySelector(".controls")?.replaceChildren();
        }
    }
    function visibility(): void {
        build.setLive(chrome.enabled && !document.hidden);
    }
    if (!navigator.onLine) offline();
    window.addEventListener("offline", offline);
    document.addEventListener("visibilitychange", visibility);
    stream.onmessage = (event) => {
        if (disposed) return;
        try {
            const changed = presentation.receive(event.data);
            const frame = presentation.current;
            if (frame && (changed || presentation.displays(event.data))) {
                build.receivedFrame();
                chrome.update(frame.state.state);
                if (changed) render(frame);
            }
        } catch {
            diagnostic.textContent = "Screen update could not be read";
        }
    };
    const INTERRUPTED = "Screen connection interrupted";
    stream.onopen = () => {
        if (disposed) return;
        presentation.reopened();
        // The same bridge replays the frame already showing, which draws
        // nothing: the line goes when the connection is back.
        if (diagnostic.textContent === INTERRUPTED) diagnostic.textContent = "";
    };
    stream.onerror = () => {
        if (disposed) return;
        offline();
        if (!chrome.enabled) diagnostic.textContent = INTERRUPTED;
    };
    function dispose(): void {
        if (disposed) return;
        disposed = true;
        stopAnimation?.();
        clearTimeout(clockTimer);
        clearTimeout(expiryTimer);
        clearTimeout(failureTimer);
        settings?.dispose();
        packs?.dispose();
        chrome.dispose();
        build.dispose();
        window.removeEventListener("offline", offline);
        document.removeEventListener("visibilitychange", visibility);
        stream.close();
        cancelPending?.();
        window.removeEventListener("pagehide", dispose);
    }
    window.addEventListener("pagehide", dispose);
    return dispose;
}
