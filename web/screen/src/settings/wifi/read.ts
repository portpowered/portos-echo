import { settingsUrl } from "../api";
import { element } from "../forms";

type Operation = "status" | "scan";
type Network = { ssid: string; signalDbm: number };

const unavailable = "Wi-Fi is unavailable. Try again.";

export function mountWifi(actions: string) {
    const section = element("details");
    section.className = "settings-section wifi-settings";
    section.hidden = true;
    const current = element("p");
    current.setAttribute("role", "status");
    const feedback = element("p");
    feedback.setAttribute("role", "status");
    const networks = element("ul");
    networks.setAttribute("aria-label", "Available networks");
    const retry = element("button", "Scan again");
    retry.type = "button";
    section.append(
        element("summary", "Wi-Fi"),
        current,
        feedback,
        networks,
        retry,
    );
    const controller = new AbortController();
    let scanning = false;

    async function observe() {
        try {
            const status = await request(actions, "status", controller.signal);
            if (!status.ok && status.error === "wifi-unsupported") return false;
            if (status.ok !== true || typeof status.reachable !== "boolean")
                throw new Error(unavailable);
            if (typeof status.ssid === "string") {
                const reachability = status.reachable
                    ? "reachable"
                    : "unreachable";
                current.textContent = `Current network: ${status.ssid} (${reachability})`;
            } else {
                current.textContent = "No current network (unreachable)";
            }
        } catch {
            current.textContent = unavailable;
        }
        return true;
    }

    async function scan(refreshStatus: boolean) {
        if (scanning || !section.open) return;
        scanning = true;
        networks.replaceChildren();
        feedback.textContent = "Scanning for networks…";
        retry.disabled = true;
        try {
            if (refreshStatus) await observe();
            const reply = await request(actions, "scan", controller.signal);
            if (!reply.ok || !Array.isArray(reply.networks))
                throw new Error(unavailable);
            const rows = reply.networks.filter(isNetwork);
            for (const row of rows)
                networks.append(
                    element("li", `${row.ssid} (${row.signalDbm} dBm)`),
                );
            feedback.textContent = rows.length ? "" : "No networks found.";
        } catch {
            feedback.textContent = unavailable;
        } finally {
            scanning = false;
            retry.disabled = false;
        }
    }

    section.addEventListener("toggle", () => void scan(false));
    retry.addEventListener("click", () => void scan(true));
    current.textContent = "Reading Wi-Fi status…";
    void observe().then((supported) => {
        if (!controller.signal.aborted) section.hidden = !supported;
    });
    return { element: section, stop: () => controller.abort() };
}

function isNetwork(value: unknown): value is Network {
    return (
        !!value &&
        typeof value === "object" &&
        "ssid" in value &&
        typeof value.ssid === "string" &&
        "signalDbm" in value &&
        Number.isInteger(value.signalDbm)
    );
}

async function request(actions: string, op: Operation, signal: AbortSignal) {
    const url = new URL(settingsUrl(actions));
    url.pathname = "/_screen/wifi";
    const result = await fetch(url, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
            type: "wifi-request",
            data: { version: 1, op },
        }),
        redirect: "error",
        referrerPolicy: "no-referrer",
        signal,
    });
    if (!result.ok) throw new Error(unavailable);
    const frame: unknown = await result.json();
    if (
        !frame ||
        typeof frame !== "object" ||
        !("type" in frame) ||
        frame.type !== "wifi-result" ||
        !("data" in frame) ||
        !frame.data ||
        typeof frame.data !== "object" ||
        !("version" in frame.data) ||
        frame.data.version !== 1 ||
        !("op" in frame.data) ||
        frame.data.op !== op ||
        !("ok" in frame.data)
    )
        throw new Error(unavailable);
    return frame.data as Record<string, unknown>;
}
