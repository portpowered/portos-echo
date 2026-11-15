import { decodeFrame } from "./decoder";
import type { ScreenFrame } from "./model";

export class Presentation {
    current: ScreenFrame | undefined;
    receive(wire: string): boolean {
        const next = decodeFrame(wire);
        if (this.current && next.seq <= this.current.seq) return false;
        this.current = next;
        return true;
    }
}
