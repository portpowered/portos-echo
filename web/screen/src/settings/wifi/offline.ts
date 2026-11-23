import type { WifiExchange } from "./read";

const pollMs = 1000;
const outageMs = 30000;

export function watchWifi(request: WifiExchange, open: () => void): () => void {
    const lifetime = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    let deadline: number | undefined;
    let prompted = false;

    async function poll() {
        const signal = AbortSignal.any([
            lifetime.signal,
            AbortSignal.timeout(5000),
        ]);
        const status = await request({ op: "status" }, signal).catch(
            () => undefined,
        );
        if (lifetime.signal.aborted) return;
        const now = performance.now();
        if (
            status?.ok !== true ||
            typeof status.reachable !== "boolean" ||
            (typeof status.ssid !== "string" &&
                !(status.ssid === null && !status.reachable))
        ) {
            deadline = undefined;
        } else if (status.reachable) {
            deadline = undefined;
            prompted = false;
        } else if (!prompted) {
            deadline ??= now + outageMs;
            if (now >= deadline) {
                prompted = true;
                deadline = undefined;
                open();
            }
        }
        timer = setTimeout(
            () => void poll(),
            deadline === undefined ? pollMs : Math.min(pollMs, deadline - now),
        );
    }

    void poll();
    return () => {
        lifetime.abort();
        clearTimeout(timer);
    };
}
