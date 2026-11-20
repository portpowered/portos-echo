import { test } from "node:test";
import assert from "node:assert/strict";
import { Player, stateName, source } from "../../dist/lib/packs/player.js";

const clip = (name, mode = "loop") => ({
    webm: `${name}.webm`,
    mp4: `${name}.mp4`,
    mode,
});
function harness(character) {
    const calls = [];
    const player = new Player(character, (value) => calls.push(value));
    return { player, calls, end: () => player.ended(calls.at(-1).ticket) };
}

test("pack player: exact states, loop boundaries, urgent cuts and coalesced transitions", () => {
    const { player, calls, end } = harness({
        rest: "rest.png",
        states: Object.fromEntries(
            [
                "idle",
                "listening",
                "thinking",
                "speaking",
                "approval",
                "error",
            ].map((s) => [s, { clip: clip(s) }]),
        ),
    });
    player.update({ state: "ready" });
    assert.equal(calls.at(-1).clip.webm, "idle.webm");
    player.update({ state: "listening" });
    assert.equal(calls.at(-1).clip.webm, "listening.webm");
    for (const state of ["thinking", "speaking", "ready"]) {
        const count = calls.length;
        player.update({ state });
        assert.equal(calls.length, count);
        end();
        assert.equal(
            calls.at(-1).clip.webm,
            `${state === "ready" ? "idle" : state}.webm`,
        );
    }
    end();
    assert.equal(calls.at(-1).clip.webm, "idle.webm");
    player.update({ state: "thinking" });
    player.update({ state: "speaking" });
    end();
    assert.equal(calls.at(-1).clip.webm, "speaking.webm");
    for (const state of ["approval", "error"]) {
        player.update({ state });
        assert.equal(calls.at(-1).clip.webm, `${state}.webm`);
    }
    assert.equal(stateName({ state: "connected" }), "idle");
    assert.equal(stateName({ state: "link_code" }), "setup");
});

test("pack player: emote once, unknown frames fallback, idle/rest fallback and once state", () => {
    const { player, calls, end } = harness({
        rest: "rest.png",
        states: { idle: { clip: clip("idle") } },
        emotes: { wave: clip("wave") },
    });
    player.update({ state: "muted" });
    assert.equal(calls.at(-1).clip.webm, "idle.webm");
    assert.equal(player.update({ state: "emotion", name: "wave" }), true);
    assert.equal(calls.at(-1).clip.webm, "wave.webm");
    end();
    assert.equal(calls.at(-1).clip.webm, "idle.webm");
    const count = calls.length;
    assert.equal(player.update({ state: "emotion", name: "missing" }), false);
    assert.equal(calls.length, count);
    assert.equal(player.update({ state: "emotion", name: "toString" }), false);
    const rest = harness({
        rest: "rest.png",
        states: { speaking: { clip: clip("speaking", "once") } },
    });
    rest.player.update({ state: "ready" });
    assert.equal(rest.calls.at(-1).clip, undefined);
    rest.player.update({ state: "speaking" });
    assert.equal(rest.calls.at(-1).clip.webm, "speaking.webm");
    rest.end();
    assert.equal(rest.calls.at(-1).clip, undefined);
    rest.player.update({ state: "ready" });
    assert.equal(rest.calls.at(-1).clip, undefined);
});

test("pack player: enter/exit once, stale completion refused and reduced-motion state updates", () => {
    const { player, calls, end } = harness({
        rest: "rest.png",
        states: {
            idle: { clip: clip("idle"), exit: clip("idle-exit") },
            thinking: { clip: clip("thinking"), enter: clip("thinking-enter") },
            listening: {
                clip: clip("listening"),
                enter: clip("listening-enter"),
            },
        },
    });
    player.update({ state: "ready" });
    const stale = calls.at(-1).ticket;
    player.update({ state: "thinking" });
    end();
    assert.equal(calls.at(-1).clip.webm, "idle-exit.webm");
    end();
    assert.equal(calls.at(-1).clip.webm, "thinking-enter.webm");
    player.update({ state: "listening" });
    assert.equal(calls.at(-1).clip.webm, "listening-enter.webm");
    player.ended(stale);
    assert.equal(calls.at(-1).clip.webm, "listening-enter.webm");
    end();
    assert.equal(calls.at(-1).clip.webm, "listening.webm");
    player.update({ state: "thinking" }, true);
    assert.equal(calls.at(-1).clip.webm, "thinking-enter.webm");
    assert.equal(source(clip("idle"), true), "idle.webm");
    assert.equal(source(clip("idle"), false), "idle.mp4");
    assert.equal(source({ mp4: "only.mp4", mode: "loop" }, true), "only.mp4");
    assert.equal(source({ webm: "only.webm", mode: "loop" }, false), undefined);
});
