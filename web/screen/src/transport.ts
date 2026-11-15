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
