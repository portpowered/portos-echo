// Date supports up to 8.64e15 milliseconds on either side of the epoch.
const maxDateSeconds = 8_640_000_000_000n;
const minuteMs = 60_000n;

export function formatExpiry(
    expiresAtUnix: bigint | number,
    style: "countdown" | "local-time",
): { label: string; refreshInMs?: number } {
    const seconds = BigInt(expiresAtUnix);
    if (seconds < -maxDateSeconds || seconds > maxDateSeconds)
        return { label: "Expiry unavailable" };

    const deadline = seconds * 1000n;
    if (style === "local-time")
        return {
            label:
                "Expires at " + new Date(Number(deadline)).toLocaleTimeString(),
        };

    const remaining = deadline - BigInt(Date.now());
    if (remaining <= 0n) return { label: "Expired" };
    const minutes = (remaining + minuteMs - 1n) / minuteMs;
    return {
        label: `Expires in ${minutes} min`,
        refreshInMs: Number(remaining - (minutes - 1n) * minuteMs),
    };
}
