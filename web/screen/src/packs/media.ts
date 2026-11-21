import { source, type Playback } from "./player";

// A clip change eases the eyes from where the old clip left them to where the
// new one starts, while the old clip fades out above the new one.
export const GLIDE_MS = 240;
const GLIDE_MIN_PX = 6;

// Bright-pixel centroid in the source's own pixels; undefined when unreadable.
function centroid(
    canvas: HTMLCanvasElement,
    image: CanvasImageSource,
    width: number,
    height: number,
): { x: number; y: number } | undefined {
    try {
        const context = canvas.getContext("2d", { willReadFrequently: true });
        if (!context || !width || !height) return;
        context.clearRect(0, 0, canvas.width, canvas.height);
        context.drawImage(image, 0, 0, canvas.width, canvas.height);
        const { data } = context.getImageData(
            0,
            0,
            canvas.width,
            canvas.height,
        );
        let x = 0;
        let y = 0;
        let count = 0;
        for (let i = 0; i < data.length; i += 4) {
            if (data[i + 3]! < 128) continue;
            if (Math.max(data[i]!, data[i + 1]!, data[i + 2]!) < 128) continue;
            const pixel = i / 4;
            x += pixel % canvas.width;
            y += Math.floor(pixel / canvas.width);
            count++;
        }
        if (!count) return;
        return {
            x: ((x / count + 0.5) * width) / canvas.width,
            y: ((y / count + 0.5) * height) / canvas.height,
        };
    } catch {
        return undefined;
    }
}

export function mediaStage(
    host: HTMLElement,
    still: HTMLImageElement,
    asset: (file: string) => string,
    ended: (ticket: number) => void,
    failed: () => void,
    // The stage has just put the requested clip, or the still, in view.
    revealed: () => void = () => {},
) {
    const stage = document.createElement("div");
    stage.className = "pack-stage";
    stage.dataset.packPersistent = "";
    stage.setAttribute("aria-hidden", "true");
    still.alt = "";
    still.onerror = failed;
    const videos = [
        document.createElement("video"),
        document.createElement("video"),
    ];
    for (const video of videos) {
        video.muted = true;
        video.playsInline = true;
        video.preload = "auto";
        video.hidden = true;
        video.setAttribute("referrerpolicy", "no-referrer");
    }
    stage.append(still, ...videos);
    host.prepend(stage);
    const canvas = document.createElement("canvas");
    canvas.width = 160;
    canvas.height = 100;
    let front = -1;
    let shownPath: string | undefined;
    let generation = 0;
    let stopped = false;
    let cancel = () => {};
    let settle: (() => void) | undefined;
    const motion = matchMedia("(prefers-reduced-motion: reduce)");
    let current: Playback | undefined;
    // Clip bytes are fetched once, one at a time, and replayed from memory:
    // a state change then never waits for the network or the pack's reader.
    const fetched = new Map<string, string>();
    let fetching = Promise.resolve();

    function warm(path: string): void {
        if (fetched.has(path)) return;
        fetched.set(path, "");
        fetching = fetching.then(async () => {
            try {
                if (stopped) return;
                const response = await fetch(asset(path), {
                    cache: "no-store",
                    credentials: "omit",
                    referrerPolicy: "no-referrer",
                });
                if (response.status !== 200) throw new Error("Unavailable");
                const url = URL.createObjectURL(await response.blob());
                if (stopped) URL.revokeObjectURL(url);
                else fetched.set(path, url);
            } catch {
                fetched.delete(path);
            }
        });
    }

    // The still is visible only while no clip is: it never sits under a clip.
    function rest(): void {
        settle?.();
        for (const video of videos) {
            video.pause();
            video.hidden = true;
        }
        still.hidden = false;
        front = -1;
        shownPath = undefined;
    }

    function offset(from: HTMLVideoElement, to: HTMLVideoElement) {
        const a = centroid(canvas, from, from.videoWidth, from.videoHeight);
        const b = centroid(canvas, to, to.videoWidth, to.videoHeight);
        if (!a || !b) return { x: 0, y: 0 };
        const scale = Math.min(
            to.clientWidth / to.videoWidth,
            to.clientHeight / to.videoHeight,
        );
        const x = (a.x - b.x) * scale;
        const y = (a.y - b.y) * scale;
        return Math.hypot(x, y) < GLIDE_MIN_PX ? { x: 0, y: 0 } : { x, y };
    }

    function ease(from: HTMLVideoElement, to: HTMLVideoElement): void {
        from.pause();
        if (typeof to.animate !== "function") {
            from.hidden = true;
            return;
        }
        const { x, y } = offset(from, to);
        const timing = { duration: GLIDE_MS, easing: "ease-out" };
        from.style.zIndex = "1";
        from.dataset.leaving = "";
        const incoming = to.animate(
            [{ transform: `translate(${x}px, ${y}px)` }, { transform: "none" }],
            timing,
        );
        const outgoing = from.animate(
            [
                { transform: "none", opacity: 1 },
                { transform: `translate(${-x}px, ${-y}px)`, opacity: 0 },
            ],
            { ...timing, fill: "forwards" },
        );
        const done = () => {
            if (settle !== done) return;
            settle = undefined;
            incoming.cancel();
            outgoing.cancel();
            from.hidden = true;
            from.style.zIndex = "";
            delete from.dataset.leaving;
        };
        settle = done;
        outgoing.onfinish = done;
    }

    function show(playback: Playback): void {
        current = playback;
        cancel();
        settle?.();
        const own = ++generation;
        if (stopped) return;
        if (!playback.clip || motion.matches) {
            rest();
            revealed();
            return;
        }
        const index = front === 0 ? 1 : 0;
        const video = videos[index]!;
        const vp9 = !!video.canPlayType('video/webm; codecs="vp9"');
        const path = source(playback.clip, vp9);
        for (const clip of playback.warm ?? []) {
            const ahead = source(clip, vp9);
            if (ahead) warm(ahead);
        }
        if (!path) {
            failed();
            return;
        }
        video.pause();
        video.hidden = true;
        let frame: number | undefined;
        const valid = () => !stopped && generation === own;
        const reveal = () => {
            if (!valid()) return;
            const previous = front !== -1 && front !== index;
            video.hidden = false;
            still.hidden = true;
            if (previous) {
                const old = videos[front]!;
                // A replayed loop joins on its own seam; a new clip eases in.
                if (shownPath !== path) ease(old, video);
                else {
                    old.hidden = true;
                    old.pause();
                }
            }
            front = index;
            shownPath = path;
            revealed();
        };
        video.onloadeddata = () => {
            if (!valid()) return;
            if (video.requestVideoFrameCallback)
                frame = video.requestVideoFrameCallback(reveal);
            else reveal();
            void video.play().catch(() => {
                if (valid()) failed();
            });
        };
        video.onended = () => {
            if (valid()) ended(playback.ticket);
        };
        video.onerror = () => {
            if (valid()) failed();
        };
        // The clip's own URL names it, whichever copy of its bytes plays.
        video.dataset.clip = asset(path);
        video.src = fetched.get(path) || video.dataset.clip;
        warm(path);
        video.load();
        cancel = () => {
            video.onloadeddata = video.onended = video.onerror = null;
            if (frame !== undefined) video.cancelVideoFrameCallback(frame);
        };
    }
    const changed = () => {
        if (current) show(current);
    };
    motion.addEventListener("change", changed);
    return {
        show,
        dispose() {
            stopped = true;
            ++generation;
            cancel();
            settle?.();
            motion.removeEventListener("change", changed);
            for (const video of videos) {
                video.pause();
                video.removeAttribute("src");
                video.load();
            }
            for (const url of fetched.values())
                if (url) URL.revokeObjectURL(url);
            fetched.clear();
            still.onerror = null;
            stage.remove();
        },
    };
}
