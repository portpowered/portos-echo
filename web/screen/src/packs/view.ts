import type { UiState } from "../model";
import { decodePacks, packRequest, packUrl } from "../settings/packs-api";
import { Player, type Character } from "./player";
import { mediaStage } from "./media";
import { applyLayout, type Layout } from "./layout";

interface Manifest {
    format: 1;
    character?: Character;
    layout?: Layout;
}

export function mountPacks(
    host: HTMLElement,
    actions: string,
    stream: EventSource,
    redraw: () => void,
) {
    let stopped = false;
    let pending: AbortController | undefined;
    let cleanup: (() => void) | undefined;
    let player: Player | undefined;
    let layout: ReturnType<typeof applyLayout> | undefined;
    let state: UiState = { state: "ready", message: "" };
    let revision = -1;
    let refreshNeeded = false;
    let openedOnce = false;
    function clear() {
        const changed = host.classList.contains("screen-pack");
        cleanup?.();
        cleanup = undefined;
        player = undefined;
        layout = undefined;
        host.classList.remove("screen-pack");
        return changed;
    }
    async function reload() {
        pending?.abort();
        // Settings owns the visible surface and its foreground reads. Coalesce
        // named changes until Back exposes the presentation, rather than race
        // its selection receipt/catalog or preload media behind the panel.
        if (host.hidden) {
            refreshNeeded = true;
            return;
        }
        refreshNeeded = false;
        const controller = new AbortController();
        pending = controller;
        try {
            const catalog = decodePacks(
                await packRequest(actions, "", controller.signal),
                actions,
            );
            if (controller.signal.aborted || stopped) return;
            revision = catalog.revision;
            if (!catalog.active) {
                if (clear()) redraw();
                return;
            }
            const base = packUrl(actions, `/${catalog.active}/`);
            const asset = (file: string) => {
                if (
                    !/^[a-zA-Z0-9_./-]+$/.test(file) ||
                    file.startsWith("/") ||
                    file.split("/").some((p) => !p || p === "." || p === "..")
                )
                    throw new Error("Invalid pack file");
                const url = new URL(base);
                url.pathname += file;
                // Bypass cached bytes on hot edits; the server still revalidates.
                url.searchParams.set("revision", `${catalog.revision}`);
                return url.href;
            };
            const response = await fetch(asset("pack.json"), {
                signal: controller.signal,
                cache: "no-store",
                credentials: "omit",
                referrerPolicy: "no-referrer",
            });
            if (response.status !== 200) throw new Error("Pack unavailable");
            // The authenticated server validates the entire manifest and its files
            // on every read. Never fetch destinations supplied by catalog data.
            const manifest = (await response.json()) as Manifest;
            if (
                manifest.format !== 1 ||
                (!manifest.layout && !manifest.character)
            )
                throw new Error("Invalid pack");
            // Keep the prior presentation until the replacement neutral image
            // has decoded. Its loaded pixels remain underneath both videos.
            let still: HTMLImageElement | undefined;
            if (manifest.character) {
                still = document.createElement("img");
                still.referrerPolicy = "no-referrer";
                still.src = asset(manifest.character.rest);
                await still.decode();
            }
            if (controller.signal.aborted || stopped) return;
            clear();
            host.classList.add("screen-pack");
            layout = applyLayout(host, manifest.layout);
            const ownedLayout = layout;
            const css = document.createElement("link");
            css.rel = "stylesheet";
            css.referrerPolicy = "no-referrer";
            if (manifest.layout?.css) {
                css.onload = () => ownedLayout.refresh();
                css.href = asset(manifest.layout.css);
                // Not in the host: every frame re-inserts the host's children,
                // and a re-inserted link drops its sheet until fetched again.
                document.head.append(css);
            }
            let media: ReturnType<typeof mediaStage> | undefined;
            cleanup = () => {
                media?.dispose();
                ownedLayout.dispose();
                css.remove();
            };
            if (manifest.character && still) {
                media = mediaStage(
                    host,
                    still,
                    asset,
                    (ticket) => player?.ended(ticket),
                    () => {
                        clear();
                        redraw();
                    },
                );
                const ownedMedia = media;
                player = new Player(manifest.character, (value) =>
                    ownedMedia.show(value),
                );
                layout.stage();
            }
            redraw();
        } catch {
            if (!controller.signal.aborted && !stopped) {
                if (clear()) redraw();
            }
        } finally {
            if (pending === controller) pending = undefined;
        }
    }
    const changed = (event: MessageEvent) => {
        try {
            const data = JSON.parse(event.data);
            if (
                data.version === 1 &&
                Number.isSafeInteger(data.revision) &&
                data.revision > revision
            )
                void reload();
        } catch {
            /* A malformed named event never mutates the ordinary frame. */
        }
    };
    const opened = () => {
        if (!openedOnce) {
            openedOnce = true;
            // Mount already starts this read. Cancelling and replacing it on
            // the first SSE open races its still-running bounded disk scan.
            if (pending && !pending.signal.aborted) return;
        }
        void reload();
    };
    const visibility = new MutationObserver(() => {
        if (!host.hidden && refreshNeeded) void reload();
    });
    visibility.observe(host, { attributes: true, attributeFilter: ["hidden"] });
    stream.addEventListener("screen-pack-reload", changed as EventListener);
    stream.addEventListener("open", opened);
    void reload();
    return {
        update(next: UiState): boolean {
            state = next;
            layout?.render(state.state);
            const handled =
                player?.update(
                    state,
                    matchMedia("(prefers-reduced-motion: reduce)").matches,
                ) ?? false;
            const stage = host.querySelector<HTMLElement>(".pack-stage");
            if (stage) stage.hidden = state.state === "emotion" && !handled;
            return handled;
        },
        dispose() {
            stopped = true;
            pending?.abort();
            clear();
            visibility.disconnect();
            stream.removeEventListener(
                "screen-pack-reload",
                changed as EventListener,
            );
            stream.removeEventListener("open", opened);
        },
    };
}
