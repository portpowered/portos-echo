import { decodeFrame } from "./decoder";
import type { ScreenFrame } from "./model";

export class Presentation {
    current: ScreenFrame | undefined;
    private wire = "";
    private fresh = false;
    // The events stream opened. A bridge that restarted counts its frames
    // from one again, so the first frame of a connection is judged by what it
    // says, not by its number: only the frame already showing is a replay.
    reopened(): void {
        this.fresh = true;
    }
    displays(wire: string): boolean {
        return this.current !== undefined && this.wire === wire;
    }
    receive(wire: string): boolean {
        const next = decodeFrame(wire);
        const replay = this.fresh
            ? wire === this.wire
            : !!this.current && next.seq <= this.current.seq;
        this.fresh = false;
        if (replay) return false;
        this.current = next;
        this.wire = wire;
        return true;
    }
}
