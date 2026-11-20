import { source, type Playback } from "./player";

export function mediaStage(
    host: HTMLElement,
    still: HTMLImageElement,
    asset: (file: string) => string,
    ended: (ticket: number) => void,
    failed: () => void,
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
    let front = -1;
    let generation = 0;
    let stopped = false;
    let cancel = () => {};
    const motion = matchMedia("(prefers-reduced-motion: reduce)");
    let current: Playback | undefined;

    function show(playback: Playback): void {
        current = playback;
        cancel();
        const own = ++generation;
        if (stopped) return;
        if (!playback.clip || motion.matches) {
            for (const video of videos) {
                video.pause();
                video.hidden = true;
            }
            front = -1;
            return;
        }
        const index = front === 0 ? 1 : 0;
        const video = videos[index]!;
        const path = source(
            playback.clip,
            !!video.canPlayType('video/webm; codecs="vp9"'),
        );
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
            video.hidden = false;
            if (front !== -1 && front !== index) {
                videos[front]!.hidden = true;
                videos[front]!.pause();
            }
            front = index;
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
        video.src = asset(path);
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
            motion.removeEventListener("change", changed);
            for (const video of videos) {
                video.pause();
                video.removeAttribute("src");
                video.load();
            }
            still.onerror = null;
            stage.remove();
        },
    };
}
