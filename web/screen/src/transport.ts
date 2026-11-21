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

// The browser opens a lost events stream again by itself, every few seconds,
// but not a stream the bridge answered with an error status (a bridge still
// stopping, a full client table, a refused pairing): that one stays closed,
// and the screen would show "Screen connection interrupted" until the app
// restarts. Events opens a new stream then, REOPEN_MS[n] after the nth such
// closure in a row (the last delay repeats), and keeps every listener and
// handler on the stream that replaces it. An opened stream resets the count.
export const REOPEN_MS = [1000, 2000, 4000];
const CLOSED = 2;

export interface StreamEvents {
    addEventListener(type: string, listener: EventListener): void;
    removeEventListener(type: string, listener: EventListener): void;
}

export class Events implements StreamEvents {
    onopen: (() => void) | null = null;
    onmessage: ((event: MessageEvent) => void) | null = null;
    onerror: (() => void) | null = null;
    private source!: EventSource;
    private listeners: [string, EventListener][] = [];
    private closures = 0;
    private timer: ReturnType<typeof setTimeout> | undefined;
    private stopped = false;

    constructor(private url: string) {
        this.connect();
    }

    addEventListener(type: string, listener: EventListener): void {
        this.listeners.push([type, listener]);
        this.source.addEventListener(type, listener);
    }

    removeEventListener(type: string, listener: EventListener): void {
        this.listeners = this.listeners.filter(
            ([name, own]) => name !== type || own !== listener,
        );
        this.source.removeEventListener(type, listener);
    }

    close(): void {
        this.stopped = true;
        clearTimeout(this.timer);
        this.timer = undefined;
        this.source.close();
    }

    private connect(): void {
        const source = new EventSource(this.url);
        this.source = source;
        for (const [type, listener] of this.listeners)
            source.addEventListener(type, listener);
        source.onopen = () => {
            if (source !== this.source) return;
            this.closures = 0;
            this.onopen?.();
        };
        source.onmessage = (event) => {
            if (source === this.source) this.onmessage?.(event);
        };
        source.onerror = () => {
            if (source !== this.source || this.stopped) return;
            this.onerror?.();
            if (source.readyState !== CLOSED || this.timer !== undefined)
                return;
            const delay =
                REOPEN_MS[Math.min(this.closures++, REOPEN_MS.length - 1)];
            this.timer = setTimeout(() => {
                this.timer = undefined;
                if (this.stopped) return;
                source.close();
                this.connect();
            }, delay);
        };
    }
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
    stream: StreamEvents,
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
