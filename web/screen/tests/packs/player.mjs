import { test } from "node:test";
import assert from "node:assert/strict";
import {
    Player,
    stateName,
    source,
    SPEAK_START_MS,
    SPEAK_HOLD_MS,
} from "../../dist/lib/packs/player.js";

const clip = (name, mode = "loop") => ({
    webm: `${name}.webm`,
    mp4: `${name}.mp4`,
    mode,
});
// A manual clock: timers run only when the test advances it.
function harness(character) {
    const calls = [];
    let now = 0;
    let timers = [];
    const schedule = (run, ms) => {
        const timer = { at: now + ms, run };
        timers.push(timer);
        return () => {
            timers = timers.filter((t) => t !== timer);
        };
    };
    const player = new Player(
        character,
        (value) => calls.push(value),
        schedule,
    );
    const advance = (ms) => {
        now += ms;
        for (const timer of timers.filter((t) => t.at <= now)) {
            timers = timers.filter((t) => t !== timer);
            timer.run();
        }
    };
    const shown = () => calls.at(-1).clip?.webm;
    return {
        player,
        calls,
        advance,
        shown,
        end: () => player.ended(calls.at(-1).ticket),
    };
}
const states = (names) =>
    Object.fromEntries(names.map((s) => [s, { clip: clip(s) }]));

test("pack player: every state cuts at once, without waiting for the clip to end", () => {
    const { player, shown, end, advance } = harness({
        rest: "rest.png",
        states: states([
            "idle",
            "wake",
            "listening",
            "thinking",
            "speaking",
            "approval",
            "error",
            "muted",
        ]),
    });
    player.update({ state: "ready" });
    assert.equal(shown(), "idle.webm");
    for (const [state, expected] of [
        ["listening", "listening.webm"],
        ["thinking", "thinking.webm"],
        ["connected", "wake.webm"],
        ["muted", "muted.webm"],
        ["ready", "idle.webm"],
        ["approval", "approval.webm"],
        ["error", "error.webm"],
    ]) {
        player.update({ state });
        assert.equal(shown(), expected, `${state} shows at once`);
    }
    // Loops replay on their own ended event.
    end();
    assert.equal(shown(), "error.webm");
    // Speaking reached from idle mid-loop starts after SPEAK_START_MS: the
    // 16 s idle loop never holds the mouth back.
    player.update({ state: "ready" });
    player.update({ state: "speaking" });
    assert.equal(shown(), "idle.webm");
    advance(SPEAK_START_MS);
    assert.equal(shown(), "speaking.webm");
    assert.equal(stateName({ state: "connected" }), "wake");
    assert.equal(stateName({ state: "ready" }), "idle");
    assert.equal(stateName({ state: "link_code" }), "setup");
});

test("pack player: the mouth moves through short reply gaps and closes once after a long silence", () => {
    const { player, calls, shown, advance } = harness({
        rest: "rest.png",
        states: states(["idle", "wake", "listening", "speaking"]),
    });
    player.update({ state: "listening" });
    player.update({ state: "connected" });
    assert.equal(shown(), "wake.webm");
    // A blip of audio shorter than the start delay never opens the mouth.
    player.update({ state: "speaking" });
    advance(SPEAK_START_MS - 20);
    player.update({ state: "connected" });
    advance(SPEAK_HOLD_MS * 2);
    assert.equal(shown(), "wake.webm");
    player.update({ state: "speaking" });
    advance(SPEAK_START_MS);
    assert.equal(shown(), "speaking.webm");
    const started = calls.length;
    // One reply with several gaps under the hold: the mouth keeps moving.
    for (const gap of [120, 300, 480, 250]) {
        player.update({ state: "connected" });
        advance(gap);
        player.update({ state: "speaking" });
        advance(400);
    }
    assert.equal(calls.length, started, "no clip change during short gaps");
    assert.equal(shown(), "speaking.webm");
    // A 1.5 s silence closes the mouth exactly once, after the hold.
    player.update({ state: "connected" });
    advance(SPEAK_HOLD_MS - 1);
    assert.equal(shown(), "speaking.webm");
    advance(1500 - SPEAK_HOLD_MS + 1);
    assert.equal(shown(), "wake.webm");
    assert.equal(calls.length, started + 1);
    // Listening (barge-in) cuts the mouth at once.
    player.update({ state: "speaking" });
    advance(SPEAK_START_MS);
    player.update({ state: "listening" });
    assert.equal(shown(), "listening.webm");
    // Run end: the awake face returns to idle with no mouth.
    player.update({ state: "speaking" });
    advance(SPEAK_START_MS);
    player.update({ state: "ready" });
    advance(SPEAK_HOLD_MS);
    assert.equal(shown(), "idle.webm");
});

test("pack player: an open session shows listening whenever she is not speaking", () => {
    // A pack with no wake clip, as Companion.
    const { player, calls, shown, advance } = harness({
        rest: "rest.png",
        states: states(["idle", "listening", "thinking", "speaking"]),
    });
    player.update({ state: "ready" });
    assert.equal(shown(), "idle.webm");
    // First turn: the wake, then the call connects while she still listens.
    player.update({ state: "listening" });
    assert.equal(shown(), "listening.webm");
    const listening = calls.length;
    player.update({ state: "connected" });
    assert.equal(calls.length, listening, "the listening clip plays on");
    player.update({ state: "speaking" });
    advance(SPEAK_START_MS);
    assert.equal(shown(), "speaking.webm");
    // The reply ends and the session waits for the next utterance.
    player.update({ state: "connected" });
    advance(SPEAK_HOLD_MS);
    assert.equal(shown(), "listening.webm");
    // Second turn, with a delegated task in it.
    player.update({ state: "thinking" });
    assert.equal(shown(), "thinking.webm");
    player.update({ state: "speaking" });
    advance(SPEAK_START_MS);
    assert.equal(shown(), "speaking.webm");
    player.update({ state: "connected" });
    advance(SPEAK_HOLD_MS);
    assert.equal(shown(), "listening.webm");
    // The session closes.
    player.update({ state: "ready" });
    assert.equal(shown(), "idle.webm");
});

test("pack player: emote once, unknown frames fallback, idle/rest fallback and once state", () => {
    const { player, calls, end, shown } = harness({
        rest: "rest.png",
        states: { idle: { clip: clip("idle") } },
        emotes: { wave: clip("wave") },
    });
    player.update({ state: "muted" });
    assert.equal(shown(), "idle.webm");
    // A pack with neither a wake nor a listening clip keeps idle for Connected.
    const count = calls.length;
    player.update({ state: "connected" });
    assert.equal(calls.length, count);
    assert.equal(player.update({ state: "emotion", name: "wave" }), true);
    assert.equal(shown(), "wave.webm");
    end();
    assert.equal(shown(), "idle.webm");
    const after = calls.length;
    assert.equal(player.update({ state: "emotion", name: "missing" }), false);
    assert.equal(calls.length, after);
    assert.equal(player.update({ state: "emotion", name: "toString" }), false);
    const rest = harness({
        rest: "rest.png",
        states: { speaking: { clip: clip("speaking", "once") } },
    });
    rest.player.update({ state: "ready" });
    assert.equal(rest.calls.at(-1).clip, undefined);
    rest.player.update({ state: "speaking" });
    rest.advance(SPEAK_START_MS);
    assert.equal(rest.shown(), "speaking.webm");
    rest.end();
    assert.equal(rest.calls.at(-1).clip, undefined);
    rest.player.update({ state: "ready" });
    rest.advance(SPEAK_HOLD_MS);
    assert.equal(rest.calls.at(-1).clip, undefined);
});

test("pack player: enter/exit once, stale completion refused and reduced-motion state updates", () => {
    const { player, calls, end, shown } = harness({
        rest: "rest.png",
        states: {
            idle: { clip: clip("idle"), exit: clip("idle-exit") },
            thinking: { clip: clip("thinking"), enter: clip("thinking-enter") },
            listening: {
                clip: clip("listening"),
                enter: clip("listening-enter"),
            },
            speaking: { clip: clip("speaking") },
        },
    });
    player.update({ state: "ready" });
    const stale = calls.at(-1).ticket;
    player.update({ state: "thinking" });
    assert.equal(shown(), "idle-exit.webm");
    end();
    assert.equal(shown(), "thinking-enter.webm");
    player.update({ state: "listening" });
    assert.equal(shown(), "listening-enter.webm");
    player.ended(stale);
    assert.equal(shown(), "listening-enter.webm");
    end();
    assert.equal(shown(), "listening.webm");
    player.update({ state: "thinking" }, true);
    assert.equal(shown(), "thinking-enter.webm");
    // Reduced motion applies every state at once, with no speaking timers.
    player.update({ state: "speaking" }, true);
    assert.equal(shown(), "speaking.webm");
    player.update({ state: "connected" }, true);
    assert.equal(shown(), "listening-enter.webm");
    assert.equal(source(clip("idle"), true), "idle.webm");
    assert.equal(source(clip("idle"), false), "idle.mp4");
    assert.equal(source({ mp4: "only.mp4", mode: "loop" }, true), "only.mp4");
    assert.equal(source({ webm: "only.webm", mode: "loop" }, false), undefined);
});
