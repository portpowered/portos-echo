import { test } from "node:test";
import assert from "node:assert/strict";
import { Presentation } from "../dist/lib/state.js";
import { frame, states } from "./fixtures.mjs";

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

test("a restarted bridge's first frame replaces a higher-numbered one", () => {
    const state = new Presentation();
    state.reopened();
    assert.equal(state.receive(frame(40, states[0])), true);
    // The same connection: a lower number is stale.
    assert.equal(state.receive(frame(2, states[3])), false);
    // The stream opens again on the same bridge, which replays its snapshot.
    state.reopened();
    assert.equal(state.receive(frame(40, states[0])), false);
    assert.equal(state.receive(frame(41, states[3])), true);
    // The bridge restarted and counts from one; its frames are shown.
    state.reopened();
    assert.equal(state.receive(frame(1, states[0])), true);
    assert.equal(state.current.seq, 1n);
    assert.equal(state.receive(frame(2, states[5])), true);
    assert.equal(state.receive(frame(2, states[3])), false);
    // A malformed frame is not a connection's first frame.
    state.reopened();
    assert.throws(() => state.receive("broken"));
    assert.equal(state.receive(frame(1, states[0])), true);
    assert.equal(state.current.seq, 1n);
});
