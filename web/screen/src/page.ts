import type { ScreenFrame, UiAction } from "./model";
import { Presentation } from "./state";
import { endpoints, postAction } from "./transport";
import { animate } from "./emotion";
import { mountSettings } from "./settings/view";

function text(tag: string, value: string): HTMLElement {
    const node = document.createElement(tag);
    node.textContent = value;
    return node;
}

export function startPage(root: HTMLElement, href: string): () => void {
    const content = document.createElement("section");
    content.setAttribute("aria-label", "Screen presentation");
    const diagnostic = text("p", "");
    diagnostic.setAttribute("role", "status");
    root.replaceChildren(content, diagnostic);
    const presentation = new Presentation();
    let stream: EventSource;
    let urls: ReturnType<typeof endpoints>;
    let pending: AbortController | undefined;
    let disposed = false;
    let cancelPending: (() => void) | undefined;
    let stopAnimation: (() => void) | undefined;
    let settings: ReturnType<typeof mountSettings> | undefined;
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
    function render(frame: ScreenFrame): void {
        settings?.update(frame.state);
        stopAnimation?.();
        cancelPending?.();
        const state = frame.state;
        content.replaceChildren(text("h1", state.state.replace(/_/g, " ")));
        const details = document.createElement("div");
        details.className = "details";
        const controls = document.createElement("div");
        controls.className = "controls";
        content.append(details, controls);
        content.dataset.seq = frame.seq.toString();
        diagnostic.textContent = "";
        switch (state.state) {
            case "ready":
                details.append(text("p", state.message));
                controls.append(
                    button(
                        "Start",
                        { action: "tap", target: "primary" },
                        frame,
                    ),
                );
                break;
            case "link_code":
                details.append(
                    text("p", state.user_code),
                    text("p", state.verification_uri),
                    text("p", state.expires_at_unix.toString()),
                );
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
    }
    try {
        urls = endpoints(href);
        stream = new EventSource(urls.events);
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
        settings?.dispose();
        stream.close();
        cancelPending?.();
        window.removeEventListener("pagehide", dispose);
    }
    window.addEventListener("pagehide", dispose);
    return dispose;
}
