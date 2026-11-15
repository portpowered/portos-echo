export interface Clock {
    setTimeout(callback: () => void, milliseconds: number): number;
    clearTimeout(timer: number): void;
}

// Browsers clamp longer delays to 1ms. Keep the remaining duration exact instead.
export function animate(
    frames: string[],
    duration: bigint,
    show: (frame: string) => void,
    clock: Clock = window,
): () => void {
    let stopped = false;
    let timer: number | undefined;
    let index = 0;
    show(frames[0] ?? "No animation frames");
    function schedule(remaining: bigint): void {
        const chunk = remaining > 2147483647n ? 2147483647n : remaining;
        timer = clock.setTimeout(() => {
            if (stopped) return;
            if (remaining > chunk) schedule(remaining - chunk);
            else {
                index = (index + 1) % frames.length;
                show(frames[index]!);
                schedule(duration);
            }
        }, Number(chunk));
    }
    if (frames.length > 1 && duration > 0n) schedule(duration);
    return () => {
        stopped = true;
        if (timer !== undefined) clock.clearTimeout(timer);
    };
}
