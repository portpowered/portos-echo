import { test } from "node:test";
import assert from "node:assert/strict";
import {
    decodeFrame,
    decodeState,
    decodeAction,
    encode,
} from "../dist/lib/decoder.js";
import { states, actionWires, frame } from "./fixtures.mjs";

test("all published Rust fixture states, frames and actions round trip exact bytes", () => {
    for (const wire of states) {
        assert.equal(encode(decodeState(wire)), wire);
        assert.equal(encode(decodeFrame(frame(7, wire))), frame(7, wire));
    }
    for (const wire of actionWires)
        assert.equal(encode(decodeAction(wire)), wire);
    assert.equal(
        encode(decodeAction('{"action":"dismiss","metadata":42}')),
        '{"action":"dismiss"}',
    );
    assert.throws(() => decodeAction('{"action":"unknown"}'));
});
