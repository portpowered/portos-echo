import { assertShape, readResponse, record, send, settingsUrl } from "../api";
import type { MuseProbeReasonV2, MuseProbeResultV2 } from "../model";

export const museReasons: Record<MuseProbeReasonV2, string> = {
    unconfigured: "Muse is not configured.",
    invalid_configuration: "Check the saved Muse URL, CA and credential.",
    unauthorized: "Muse did not accept the saved credential.",
    unavailable: "Muse is unavailable. Check that the service is running.",
    unexpected_response: "The server returned an unexpected Muse response.",
};

function invalid(): never {
    throw new Error("Settings response could not be read");
}

function reason(value: unknown): value is MuseProbeReasonV2 {
    return (
        typeof value === "string" &&
        Object.prototype.hasOwnProperty.call(museReasons, value)
    );
}

export function validateMuseStatus(value: unknown) {
    const data = record(value);
    assertShape(data, ["configured", "reachability"], ["url", "reason"]);
    if (typeof data.configured !== "boolean") invalid();
    if (data.configured ? typeof data.url !== "string" : "url" in data)
        invalid();
    if (
        !["unchecked", "reachable", "unreachable"].some(
            (state) => state === data.reachability,
        )
    )
        invalid();
    if (
        data.reachability === "unreachable"
            ? !reason(data.reason)
            : "reason" in data
    )
        invalid();
    if (!data.configured && data.reachability !== "unchecked") invalid();
}

export async function probeMuse(actions: string, signal: AbortSignal) {
    const response = await send(
        settingsUrl(actions, "/muse", 2),
        signal,
        "POST",
        {
            version: 2,
            operation: "probe",
        },
    );
    const data = record(await readResponse(response, 2));
    assertShape(data, ["version", "reachable"], ["reason"]);
    if (data.version !== 2 || typeof data.reachable !== "boolean") invalid();
    if (data.reachable ? "reason" in data : !reason(data.reason)) invalid();
    return data as MuseProbeResultV2;
}
