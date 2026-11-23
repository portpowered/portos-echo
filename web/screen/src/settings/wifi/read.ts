import { settingsUrl } from "../api";
import { element } from "../forms";

type WifiRequest =
    | { op: "status" | "scan" }
    | { op: "join"; ssid: string; password: string }
    | { op: "forget"; networkId: number };
type Network = {
    ssid: string;
    signalDbm: number;
    secured: boolean;
    networkId: number | null;
};

const unavailable = "Wi-Fi is unavailable. Try again.";
const mutationErrors = new Map([
    [
        "wifi-join-failed",
        "Could not join Wi-Fi. The previous network was kept.",
    ],
    [
        "wifi-rollback-failed",
        "Could not restore the previous network. Scan again or use USB recovery.",
    ],
    ["wifi-not-saved", "This network is no longer saved."],
    ["wifi-busy", "Wi-Fi is busy. Try again."],
]);

export function mountWifi(request: WifiExchange) {
    const section = element("details");
    section.className = "settings-section wifi-settings";
    section.hidden = true;
    const summary = element("summary", "Wi-Fi");
    const current = element("p");
    current.setAttribute("role", "status");
    const feedback = element("p");
    feedback.setAttribute("role", "status");
    const mutationFeedback = element("p");
    mutationFeedback.setAttribute("role", "status");
    const networks = element("ul");
    networks.setAttribute("aria-label", "Available networks");
    const retry = element("button", "Scan again");
    retry.type = "button";
    const joinSlot = element("div");
    section.append(
        summary,
        current,
        mutationFeedback,
        joinSlot,
        feedback,
        networks,
        retry,
    );
    const controller = new AbortController();
    let scanning = false;
    let mutating = false;

    function updateControls() {
        const controls = section.querySelectorAll<
            HTMLButtonElement | HTMLInputElement
        >("button, input");
        for (const control of controls) control.disabled = mutating;
        retry.disabled = scanning || mutating;
    }

    function showStatus(status: Record<string, unknown>) {
        if (typeof status.reachable !== "boolean") return false;
        const reachability = status.reachable ? "reachable" : "unreachable";
        current.textContent =
            typeof status.ssid === "string"
                ? `Current network: ${status.ssid} (${reachability})`
                : "No current network (unreachable)";
        return true;
    }

    async function observe() {
        try {
            const status = await request({ op: "status" }, controller.signal);
            if (!status.ok && status.error === "wifi-unsupported") return false;
            if (status.ok !== true || !showStatus(status))
                throw new Error(unavailable);
        } catch {
            current.textContent = unavailable;
        }
        return true;
    }

    async function scan(refreshStatus: boolean) {
        if (scanning || mutating || !section.open) return;
        scanning = true;
        networks.replaceChildren();
        feedback.textContent = "Scanning for networks…";
        retry.disabled = true;
        try {
            // Enqueue the status and scan together so a join follows both.
            if (refreshStatus) void observe();
            const reply = await request({ op: "scan" }, controller.signal);
            if (!reply.ok || !Array.isArray(reply.networks))
                throw new Error(unavailable);
            const rows = reply.networks.filter(isNetwork);
            for (const row of rows) networks.append(networkRow(row));
            feedback.textContent = rows.length ? "" : "No networks found.";
        } catch {
            feedback.textContent = unavailable;
        } finally {
            scanning = false;
            updateControls();
        }
    }

    function networkRow(row: Network) {
        const item = element("li");
        item.append(element("span", `${row.ssid} (${row.signalDbm} dBm)`));
        const join = element("button", "Join");
        join.type = "button";
        join.setAttribute("aria-label", `Join ${row.ssid}`);
        join.addEventListener("click", () => offerJoin(row));
        item.append(join);
        if (typeof row.networkId === "number") {
            const networkId = row.networkId;
            const forget = element("button", "Forget");
            forget.type = "button";
            forget.setAttribute("aria-label", `Forget ${row.ssid}`);
            forget.addEventListener(
                "click",
                () => void mutate({ op: "forget", networkId }),
            );
            item.append(forget);
        }
        return item;
    }

    function offerJoin(row: Network) {
        if (mutating) return;
        const form = element("form");
        const submit = element("button", `Join ${row.ssid}`);
        submit.type = "submit";
        const password = element("input");
        if (row.secured) {
            const label = element("label", `Password for ${row.ssid}`);
            password.type = "password";
            password.autocomplete = "off";
            password.required = true;
            password.maxLength = 64;
            label.append(password);
            form.append(label);
        }
        form.append(submit);
        form.addEventListener("submit", (event) => {
            event.preventDefault();
            if (mutating) return;
            const value = password.value;
            password.value = "";
            void mutate({ op: "join", ssid: row.ssid, password: value });
        });
        mutationFeedback.textContent = "";
        joinSlot.replaceChildren(form);
        (row.secured ? password : submit).focus();
    }

    async function mutate(
        frame: Extract<WifiRequest, { op: "join" | "forget" }>,
    ) {
        if (mutating) return;
        mutating = true;
        const hadFocus = section.contains(document.activeElement);
        updateControls();
        mutationFeedback.textContent =
            frame.op === "join" ? "Joining Wi-Fi…" : "Forgetting network…";
        let refresh = false;
        try {
            const result = await request(frame, controller.signal);
            const observed = showStatus(result);
            if (result.ok === true && observed) {
                mutationFeedback.textContent =
                    frame.op === "join"
                        ? `Connected to ${frame.ssid}`
                        : "Network forgotten.";
                joinSlot.replaceChildren();
                refresh = true;
            } else {
                const error =
                    typeof result.error === "string" ? result.error : "";
                mutationFeedback.textContent =
                    mutationErrors.get(error) ?? unavailable;
                refresh = error === "wifi-not-saved" || observed;
            }
        } catch {
            mutationFeedback.textContent = unavailable;
        } finally {
            mutating = false;
            updateControls();
            if (hadFocus && document.activeElement === document.body) {
                (
                    joinSlot.querySelector("input") ??
                    section.querySelector("summary")
                )?.focus();
            }
        }
        if (refresh) await scan(false);
    }

    section.addEventListener("toggle", () => void scan(false));
    retry.addEventListener("click", () => void scan(true));
    current.textContent = "Reading Wi-Fi status…";
    void observe().then((supported) => {
        if (!controller.signal.aborted) section.hidden = !supported;
    });
    return {
        element: section,
        open(focus: boolean) {
            section.hidden = false;
            section.open = true;
            if (focus) summary.focus();
        },
        stop: () => controller.abort(),
    };
}

function isNetwork(value: unknown): value is Network {
    return (
        !!value &&
        typeof value === "object" &&
        "ssid" in value &&
        typeof value.ssid === "string" &&
        "signalDbm" in value &&
        Number.isInteger(value.signalDbm) &&
        "secured" in value &&
        typeof value.secured === "boolean" &&
        "networkId" in value &&
        (value.networkId === null ||
            (typeof value.networkId === "number" &&
                value.networkId >= 0 &&
                Number.isInteger(value.networkId)))
    );
}

export type WifiExchange = ReturnType<typeof wifiExchange>;

export function wifiExchange(actions: string) {
    let pending = Promise.resolve();
    return (data: WifiRequest, signal: AbortSignal) => {
        // The device serializes all radio work, including status reads.
        const reply = pending.then(() => wifiRequest(actions, data, signal));
        pending = reply.then(
            () => {},
            () => {},
        );
        return reply;
    };
}

async function wifiRequest(
    actions: string,
    data: WifiRequest,
    signal: AbortSignal,
) {
    const url = new URL(settingsUrl(actions));
    url.pathname = "/_screen/wifi";
    const result = await fetch(url, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
            type: "wifi-request",
            data: { version: 1, ...data },
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
        frame.data.op !== data.op ||
        !("ok" in frame.data)
    )
        throw new Error(unavailable);
    return frame.data as Record<string, unknown>;
}
