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
}

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
        case "connected":
            return "idle";
        case "link_code":
            return "setup";
        default:
            return state.state;
    }
}

// Media events, rather than timers, drive the rest-pose boundary. Videos do not
// use native loop: ended is the documented neutral frame at either end.
export class Player {
    private state = "idle";
    private shown = "";
    private ticket = 0;
    private boundary: (() => void) | undefined;
    private completion: (() => void) | undefined;
    private emote = false;
    private playing = false;

    constructor(
        private character: Character,
        private show: (playback: Playback) => void,
    ) {}

    update(state: UiState, immediate = false): boolean {
        if (state.state === "emotion") {
            const clip = own(this.character.emotes, state.name);
            if (!clip) return false;
            this.boundary = undefined;
            this.emote = true;
            this.play(clip, () => {
                this.emote = false;
                this.loop(this.state);
            });
            return true;
        }
        const next = stateName(state);
        if (next === this.state && this.shown && !this.emote) return false;
        this.state = next;
        const urgent = ["listening", "error", "approval"].includes(next);
        if (!this.shown || !this.playing || urgent || immediate) {
            this.boundary = undefined;
            this.emote = false;
            this.enter(next);
        } else {
            // Replacing this closure coalesces states received before rest.
            this.boundary = () => {
                this.emote = false;
                const exit = this.clips(this.shown)?.exit;
                if (exit) this.play(exit, () => this.enter(this.state));
                else this.enter(this.state);
            };
        }
        return false;
    }

    ended(ticket: number): void {
        if (ticket !== this.ticket) return;
        const next = this.boundary ?? this.completion;
        this.boundary = undefined;
        next?.();
    }

    private clips(name: string): StateClip | undefined {
        return (
            own(this.character.states, name) ??
            own(this.character.states, "idle")
        );
    }

    private enter(name: string): void {
        this.shown = name;
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

    private play(clip: Clip | undefined, completion?: () => void): void {
        this.playing = clip !== undefined;
        this.completion = completion;
        this.show({ clip, ticket: ++this.ticket });
    }
}

export function source(clip: Clip, vp9: boolean): string | undefined {
    return vp9 && clip.webm ? clip.webm : clip.mp4;
}
