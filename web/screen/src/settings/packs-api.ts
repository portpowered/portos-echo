import { settingsUrl } from "./api";

export interface PackSnapshot {
    version: 1;
    revision: number;
    active: string | null;
    warning: string | null;
    installed: {
        id: string;
        name: string;
        version: string;
        rest_url: string | null;
    }[];
    jobs: { id: string; status: string; reason: string | null }[];
}

export function packUrl(actions: string, suffix = ""): string {
    const url = new URL(settingsUrl(actions));
    url.pathname = `/screen-packs/v1${suffix}`;
    return url.href;
}

// Never accept a catalog URL as a fetch destination: keep previews on the
// authenticated origin and derive their paths from validated pack identities.
export function previewUrl(actions: string, id: string, path: string): string {
    const prefix = `/screen-packs/v1/${id}/`;
    if (
        !path.startsWith(prefix) ||
        !/^[a-zA-Z0-9_./@-]+$/.test(path) ||
        path.split("/").some((part) => part === "." || part === "..") ||
        !/\.(png|webp)$/.test(path)
    )
        throw new Error("Pack response could not be read");
    const url = new URL(packUrl(actions));
    url.pathname = path;
    return url.href;
}

function object(value: unknown): Record<string, unknown> {
    if (!value || typeof value !== "object" || Array.isArray(value))
        throw new Error("Pack response could not be read");
    return value as Record<string, unknown>;
}
const identifier = (value: unknown): value is string =>
    typeof value === "string" &&
    /^[a-z][a-z0-9-]{0,47}(?:@[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.]+)?)?$/.test(
        value,
    );

export function decodePacks(value: unknown, actions: string): PackSnapshot {
    const data = object(value);
    if (
        data.version !== 1 ||
        !Number.isSafeInteger(data.revision) ||
        Number(data.revision) < 0 ||
        !(data.active === null || identifier(data.active)) ||
        !(data.warning === null || data.warning === "invalid_selection") ||
        !Array.isArray(data.installed) ||
        !Array.isArray(data.jobs) ||
        data.jobs.length > 32
    )
        throw new Error("Pack response could not be read");
    const ids = new Set<string>();
    for (const value of data.installed) {
        const pack = object(value);
        if (
            !identifier(pack.id) ||
            ids.has(pack.id) ||
            typeof pack.name !== "string" ||
            typeof pack.version !== "string" ||
            !(pack.rest_url === null || typeof pack.rest_url === "string")
        )
            throw new Error("Pack response could not be read");
        ids.add(pack.id);
        if (pack.rest_url)
            previewUrl(actions, pack.id, pack.rest_url as string);
    }
    if (data.active !== null && !ids.has(data.active as string))
        throw new Error("Pack response could not be read");
    for (const value of data.jobs) {
        const job = object(value);
        if (
            typeof job.id !== "string" ||
            !/^[0-9]+$/.test(job.id) ||
            !["queued", "downloading", "installed", "failed"].includes(
                String(job.status),
            ) ||
            !(
                job.reason === null ||
                (typeof job.reason === "string" &&
                    /^[a-z_]{1,40}$/.test(job.reason))
            )
        )
            throw new Error("Pack response could not be read");
    }
    return data as unknown as PackSnapshot;
}

export async function packRequest(
    actions: string,
    suffix: string,
    signal: AbortSignal,
    body?: unknown,
): Promise<unknown> {
    const response = await fetch(packUrl(actions, suffix), {
        method: body === undefined ? "GET" : "POST",
        ...(body === undefined
            ? {}
            : {
                  headers: { "Content-Type": "application/json" },
                  body: JSON.stringify(body),
              }),
        signal,
        cache: "no-store",
        credentials: "omit",
        referrerPolicy: "no-referrer",
    });
    if (response.status === (suffix === "/install" ? 202 : 200))
        return response.json();
    if (response.status === 401 || response.status === 403)
        throw new Error("Pairing is unavailable. Reopen the paired screen.");
    if (response.status === 503)
        throw new Error(
            "Screen packs are unavailable or busy. Refresh packs to try again.",
        );
    // Do not display arbitrary remote text or submitted download URLs.
    throw new Error(
        "Screen pack request was refused. Check the URL and SHA-256, then refresh packs.",
    );
}

export function decodeJob(value: unknown): string {
    const data = object(value);
    if (
        data.version !== 1 ||
        typeof data.job !== "string" ||
        !/^[0-9]+$/.test(data.job)
    )
        throw new Error("Pack response could not be read");
    return data.job;
}
