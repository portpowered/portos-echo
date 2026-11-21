import type { UiState } from "../model";

export interface Clip {
    webm?: string;
    mp4?: string;
    mode: "loop" | "once";
}
export interface StateClip {
    clip: Clip;
    enter?: Clip;
    exit?: Clip;
}
export interface Character {
    rest: string;
    states?: Record<string, StateClip>;
    emotes?: Record<string, Clip>;
}
export interface Playback {
    clip: Clip | undefined;
    // A ticket admits only completion of the currently requested clip.
    ticket: number;
    // Clips to fetch ahead, so a later state change never waits for one.
    warm?: Clip[];
}
export type Schedule = (run: () => void, ms: number) => () => void;

// Speaking follows reply audio, which pauses between words. The mouth starts
// once audio has lasted SPEAK_START_MS and keeps moving through gaps shorter
// than SPEAK_HOLD_MS, so short pauses never snap it shut and open again.
export const SPEAK_START_MS = 100;
export const SPEAK_HOLD_MS = 500;
const urgent = ["listening", "error", "approval", "muted", "setup"];

function own<T>(
    values: Record<string, T> | undefined,
    name: string,
): T | undefined {
    return values && Object.prototype.hasOwnProperty.call(values, name)
        ? values[name]
        : undefined;
}

export function stateName(state: UiState): string {
    switch (state.state) {
        case "ready":
            return "idle";
        // Connected is the awake face between turns: the pack's `wake` clip,
        // or idle for packs without one.
        case "connected":
            return "wake";
        case "link_code":
            return "setup";
        default:
            return state.state;
    }
}

const timer: Schedule = (run, ms) => {
    const id = setTimeout(run, ms);
    return () => clearTimeout(id);
};

// State changes take effect at once; the media stage eases between clips.
// Media events, not timers, advance a clip: loops replay on ended.
export class Player {
    private state = "idle";
    private shown = "";
    private entered = false;
    private ticket = 0;
    private completion: (() => void) | undefined;
    private emote = false;
    private pending: (() => void) | undefined;
    private warmed = false;

    constructor(
        private character: Character,
        private show: (playback: Playback) => void,
        private schedule: Schedule = timer,
    ) {}

    // The pack state whose clips are on the stage.
    get showing(): string {
        return this.shown;
    }

    update(state: UiState, immediate = false): boolean {
        if (state.state === "emotion") {
            const clip = own(this.character.emotes, state.name);
            if (!clip) return false;
            this.cancel();
            this.emote = true;
            this.play(clip, () => {
                this.emote = false;
                this.shown = "";
                this.settle(true);
            });
            return true;
        }
        this.state = stateName(state);
        const cut = immediate || urgent.includes(this.state);
        if (!this.emote || cut) {
            this.emote = false;
            this.settle(immediate);
        }
        return false;
    }

    ended(ticket: number): void {
        if (ticket !== this.ticket) return;
        this.completion?.();
    }

    private cancel(): void {
        this.pending?.();
        this.pending = undefined;
    }

    private later(ms: number): void {
        this.pending = this.schedule(() => {
            this.pending = undefined;
            this.transition(this.state);
        }, ms);
    }

    private settle(immediate: boolean): void {
        this.cancel();
        const next = this.state;
        if (next === this.shown) return;
        if (this.shown && !immediate) {
            if (next === "speaking") return this.later(SPEAK_START_MS);
            if (this.shown === "speaking" && !urgent.includes(next))
                return this.later(SPEAK_HOLD_MS);
        }
        this.transition(next);
    }

    private clips(name: string): StateClip | undefined {
        return (
            own(this.character.states, name) ??
            own(this.character.states, "idle")
        );
    }

    private transition(next: string): void {
        const previous = this.shown;
        this.shown = next;
        // A state that resolves to the same clips keeps playing undisturbed.
        if (
            previous &&
            this.entered &&
            this.clips(previous) === this.clips(next)
        )
            return;
        const exit = this.entered ? this.clips(previous)?.exit : undefined;
        this.entered = false;
        if (previous && exit) this.play(exit, () => this.enter(this.shown));
        else this.enter(next);
    }

    private enter(name: string): void {
        this.entered = true;
        const clips = this.clips(name);
        if (clips?.enter) this.play(clips.enter, () => this.loop(name));
        else this.loop(name);
    }

    private loop(name: string): void {
        const clip = this.clips(name)?.clip;
        this.play(clip, () => {
            if (clip?.mode === "loop") this.loop(name);
            else this.play(undefined);
        });
    }

    // The states a reply moves through come first: they must not lag audio.
    private all(): Clip[] {
        const states = this.character.states ?? {};
        const first = ["speaking", "wake", "idle", "listening", "thinking"];
        const names = [
            ...first.filter((name) => own(states, name)),
            ...Object.keys(states).filter((name) => !first.includes(name)),
        ];
        return [
            ...names.flatMap((name) => {
                const { clip, enter, exit } = states[name]!;
                return [clip, enter, exit].filter((c) => c !== undefined);
            }),
            ...Object.values(this.character.emotes ?? {}),
        ];
    }

    private play(clip: Clip | undefined, completion?: () => void): void {
        this.completion = completion;
        const playback: Playback = { clip, ticket: ++this.ticket };
        if (!this.warmed) playback.warm = this.all();
        this.warmed = true;
        this.show(playback);
    }
}

export function source(clip: Clip, vp9: boolean): string | undefined {
    return vp9 && clip.webm ? clip.webm : clip.mp4;
}
