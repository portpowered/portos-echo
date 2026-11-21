import type { MouthBatch, MouthSample } from "../model";
import type { Schedule } from "./player";

// Stills of the whole character, one per mouth shape. The bridge sends what
// the voice is doing; the still for the sample due now covers the speaking
// clip. The vowel shapes are optional.
export interface MouthSet {
    closed: string;
    half: string;
    open: string;
    a?: string;
    i?: string;
    u?: string;
    e?: string;
    o?: string;
}
export type MouthShape = keyof MouthSet;
export const MOUTH_SHAPES: readonly MouthShape[] = [
    "closed",
    "half",
    "open",
    "a",
    "i",
    "u",
    "e",
    "o",
];

// With no sample for this long the mouth shuts.
export const MOUTH_IDLE_MS = 150;
// Openness, 0 to 100: under HALF_AT the mouth is shut, from OPEN_AT it is
// wide open, and a vowel shape needs at least VOWEL_AT.
export const HALF_AT = 10;
export const VOWEL_AT = 30;
export const OPEN_AT = 55;
// After speaking, the shut mouth covers the speaking clip until the stage
// shows another clip. A stage that never does uncovers it after this long.
export const UNCOVER_MAX_MS = 2000;
// A sender clock this far from the last one has restarted.
const CLOCK_JUMP_MS = 2000;
const QUEUE_MAX = 64;

const timer: Schedule = (run, ms) => {
    const id = setTimeout(run, ms);
    return () => clearTimeout(id);
};

export function mouthShape(
    sample: Pick<MouthSample, "open" | "vowel">,
    has: (shape: MouthShape) => boolean,
): MouthShape {
    if (sample.open < HALF_AT) return "closed";
    if (sample.vowel !== "none" && sample.open >= VOWEL_AT && has(sample.vowel))
        return sample.vowel;
    return sample.open >= OPEN_AT ? "open" : "half";
}

// Ask the screen server for lip-sync events on this stream.
export function mouthEvents(events: string): string {
    const url = new URL(events);
    url.searchParams.set("mouth", "1");
    return url.href;
}

// Each sample is shown when its sound is heard, not when it arrives. The
// sender stamps samples on its own clock; the quickest batch seen gives the
// offset to this page's clock.
export class Lips {
    private queue: { due: number; sample: MouthSample }[] = [];
    private offset: number | undefined;
    private allowed = false;
    private shown: MouthShape | undefined;
    private applied = 0;
    private pending: (() => void) | undefined;
    private release: (() => void) | undefined;
    private leaving = false;

    constructor(
        private has: (shape: MouthShape) => boolean,
        // undefined uncovers the pack's own clip.
        private show: (shape: MouthShape | undefined) => void,
        private now: () => number = () => performance.now(),
        private schedule: Schedule = timer,
    ) {}

    receive(batch: MouthBatch): void {
        const transit = this.now() - Number(batch.sent_ms);
        const jumped =
            this.offset !== undefined &&
            Math.abs(transit - this.offset) > CLOCK_JUMP_MS;
        if (jumped) this.queue = [];
        if (this.offset === undefined || jumped || transit < this.offset)
            this.offset = transit;
        for (const sample of batch.samples) {
            const due = Number(sample.at_ms) + this.offset;
            // A sample due before ones already held replaces them: the
            // reply was cut, and their sound will never be heard.
            while (
                this.queue.length &&
                this.queue[this.queue.length - 1]!.due >= due
            )
                this.queue.pop();
            this.queue.push({ due, sample });
        }
        this.queue.splice(0, this.queue.length - QUEUE_MAX);
        this.arm();
    }

    // The pack state now showing, as the player names it.
    state(name: string): void {
        this.release?.();
        this.release = undefined;
        this.leaving = false;
        if (name === "speaking") {
            this.allowed = true;
            this.arm();
            return;
        }
        if (name === "" || this.shown === undefined) return this.stop();
        // The speaking clip stays on the stage until the next clip's first
        // frame: the mouth stays over it until then. A pause between words
        // ("wake") still takes samples; any other state shuts the mouth.
        if (name !== "wake") {
            this.pending?.();
            this.pending = undefined;
            this.allowed = false;
            this.set("closed");
        }
        this.leaving = true;
        this.release = this.schedule(() => this.stop(), UNCOVER_MAX_MS);
    }

    // The stage now shows a clip of the pack state `showing`.
    revealed(showing: string): void {
        if (this.leaving && showing !== "speaking") this.stop();
    }

    stop(): void {
        this.release?.();
        this.release = undefined;
        this.leaving = false;
        this.pending?.();
        this.pending = undefined;
        this.allowed = false;
        this.set(undefined);
    }

    private set(shape: MouthShape | undefined): void {
        if (shape === this.shown) return;
        this.shown = shape;
        this.show(shape);
    }

    private moving(): boolean {
        return this.shown !== undefined && this.shown !== "closed";
    }

    private tick(): void {
        this.pending = undefined;
        const now = this.now();
        let due: { due: number; sample: MouthSample } | undefined;
        while (this.queue.length && this.queue[0]!.due <= now)
            due = this.queue.shift();
        // A sample this stale describes sound that has already passed.
        if (due && this.allowed && now - due.due < MOUTH_IDLE_MS) {
            this.applied = now;
            this.set(mouthShape(due.sample, this.has));
        } else if (this.moving() && now - this.applied >= MOUTH_IDLE_MS)
            this.set("closed");
        this.arm();
    }

    private arm(): void {
        this.pending?.();
        this.pending = undefined;
        // Samples lead the speaking state: they wait for it here.
        if (!this.allowed) return;
        const idle = this.moving() ? this.applied + MOUTH_IDLE_MS : Infinity;
        const at = Math.min(this.queue[0]?.due ?? Infinity, idle);
        if (at === Infinity) return;
        this.pending = this.schedule(
            () => this.tick(),
            Math.max(0, at - this.now()),
        );
    }
}

// Every still is decoded before the pack shows, and stays decoded: changing
// the mouth is a visibility flip, never a load.
export async function mouthStills(
    set: MouthSet,
    asset: (file: string) => string,
) {
    const stills = new Map<MouthShape, HTMLImageElement>();
    await Promise.all(
        MOUTH_SHAPES.map(async (shape) => {
            const file = set[shape];
            if (typeof file !== "string") return;
            const still = document.createElement("img");
            still.className = "pack-mouth";
            still.alt = "";
            still.referrerPolicy = "no-referrer";
            still.src = asset(file);
            try {
                await still.decode();
                stills.set(shape, still);
            } catch {
                /* A shape that cannot be drawn is a shape the pack lacks. */
            }
        }),
    );
    for (const shape of ["closed", "half", "open"] as const)
        if (!stills.has(shape)) throw new Error("Mouth still unavailable");
    let stage: HTMLElement | undefined;
    return {
        has: (shape: MouthShape) => stills.has(shape),
        attach(target: HTMLElement): void {
            stage = target;
            target.append(...stills.values());
        },
        show(shape: MouthShape | undefined): void {
            for (const [name, still] of stills)
                still.toggleAttribute("data-shown", name === shape);
            stage?.toggleAttribute("data-lips", shape !== undefined);
        },
    };
}
