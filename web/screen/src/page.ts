import type { ScreenFrame, UiAction } from "./model";
import { Presentation } from "./state";
import { endpoints, postAction } from "./transport";

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
        const state = frame.state;
        content.replaceChildren(text("h1", state.state.replace(/_/g, " ")));
        content.dataset.seq = frame.seq.toString();
        diagnostic.textContent = "";
        switch (state.state) {
            case "ready":
                content.append(
                    text("p", state.message),
                    button(
                        "Start",
                        { action: "tap", target: "primary" },
                        frame,
                    ),
                );
                break;
            case "link_code":
                content.append(
                    text("p", state.user_code),
                    text("p", state.verification_uri),
                    text("p", state.expires_at_unix.toString()),
                );
                break;
            case "approval":
                content.append(
                    text("p", state.prompt),
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
                content.append(text("p", state.message));
                if (state.relink)
                    content.append(
                        button("Relink", { action: "relink" }, frame),
                    );
                content.append(button("Dismiss", { action: "dismiss" }, frame));
                break;
            case "emotion":
                content.append(
                    text("p", state.name),
                    text("p", state.frames[0] ?? ""),
                );
                break;
        }
    }
    try {
        urls = endpoints(href);
        stream = new EventSource(urls.events);
    } catch {
        diagnostic.textContent = "Screen configuration is invalid";
        return () => {};
    }
    stream.onmessage = (event) => {
        try {
            if (presentation.receive(event.data) && presentation.current)
                render(presentation.current);
        } catch {
            diagnostic.textContent = "Screen update could not be read";
        }
    };
    stream.onerror = () => {
        diagnostic.textContent = "Screen connection interrupted";
    };
    function dispose(): void {
        disposed = true;
        stream.close();
        cancelPending?.();
        window.removeEventListener("pagehide", dispose);
    }
    window.addEventListener("pagehide", dispose);
    return dispose;
}
