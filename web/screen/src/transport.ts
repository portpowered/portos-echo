import type { UiAction } from "./model";

export function endpoints(href: string): { events: string; actions: string } {
    const page = new URL(href);
    function resolve(key: string): string {
        const raw = page.searchParams.get(key);
        if (!raw) throw new Error("Screen configuration is invalid");
        const url = new URL(raw, page);
        if (
            !/^https?:$/.test(url.protocol) ||
            url.origin !== page.origin ||
            url.username ||
            url.password
        )
            throw new Error("Screen configuration is invalid");
        return url.href;
    }
    return { events: resolve("events"), actions: resolve("actions") };
}

export async function postAction(
    url: string,
    action: UiAction,
    signal: AbortSignal,
): Promise<void> {
    const result = await fetch(url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(action),
        signal,
    });
    if (result.status !== 204) throw new Error("Action could not be sent");
}

const BUILD_META = 'meta[name="echo-build"]';
const RELOADED = "echo-screen-reload";
const BUSY_RETRY_MS = 1000;

// The bridge embeds this page and names its build in the document. After a
// bridge upgrade a running page is the previous build: each time the events
// stream opens it reads the document again and reloads itself when the build
// named there is not its own. It reloads at most once per build, waits while
// `busy` (unsaved Settings edits) and does nothing when either side names none.
export function watchBuild(
    stream: EventSource,
    href: string,
    busy: () => boolean,
): () => void {
    const loaded = document.querySelector(BUILD_META)?.getAttribute("content");
    if (!loaded) return () => {};
    const index = new URL(href);
    index.search = index.hash = "";
    const cancel = new AbortController();
    let wanted = "";
    let timer: ReturnType<typeof setTimeout> | undefined;
    function reload(): void {
        timer = undefined;
        if (!wanted) return;
        if (busy()) {
            timer = setTimeout(reload, BUSY_RETRY_MS);
            return;
        }
        try {
            if (sessionStorage.getItem(RELOADED) === wanted) return;
            sessionStorage.setItem(RELOADED, wanted);
        } catch {
            // With no record of the reload, a second one could not be refused.
            return;
        }
        location.reload();
    }
    async function check(): Promise<void> {
        try {
            const response = await fetch(index.href, {
                cache: "no-store",
                signal: cancel.signal,
            });
            if (response.status !== 200) return;
            const served = new DOMParser()
                .parseFromString(await response.text(), "text/html")
                .querySelector(BUILD_META)
                ?.getAttribute("content");
            wanted = served && served !== loaded ? served : "";
            if (timer === undefined) reload();
        } catch {
            // The next time the stream opens asks again.
        }
    }
    const opened = () => void check();
    stream.addEventListener("open", opened);
    return () => {
        stream.removeEventListener("open", opened);
        cancel.abort();
        clearTimeout(timer);
        wanted = "";
    };
}
