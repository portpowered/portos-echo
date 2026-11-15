import { test } from "node:test";
import assert from "node:assert/strict";
import { Presentation } from "../dist/lib/state.js";
import { frame } from "./fixtures.mjs";

test("strict ordering survives malformed updates and reconnect-like replay", () => {
    const state = new Presentation();
    assert.equal(state.receive(frame(9007199254740993n)), true);
    const current = state.current;
    for (const n of [0, 9007199254740992n, 9007199254740993n])
        assert.equal(state.receive(frame(n)), false);
    assert.throws(() => state.receive("broken"));
    assert.equal(state.current, current);
    assert.equal(state.receive(frame(18446744073709551615n)), true);
    assert.equal(state.current.seq, 18446744073709551615n);
});
