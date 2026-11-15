import { test } from "node:test";
import assert from "node:assert/strict";
import {
    decodeFrame,
    decodeState,
    parseLossless,
} from "../dist/lib/decoder.js";
import { frame } from "./fixtures.mjs";

test("u64 tokens stay exact in all three integer fields", () => {
    for (const n of [
        0n,
        9007199254740991n,
        9007199254740992n,
        9007199254740993n,
        18446744073709551615n,
    ]) {
        assert.equal(decodeFrame(frame(n)).seq, n);
        assert.equal(
            decodeState(
                `{"state":"link_code","user_code":"","verification_uri":"","expires_at_unix":${n}}`,
            ).expires_at_unix,
            n,
        );
        assert.equal(
            decodeState(
                `{"state":"emotion","name":"","frames":[],"frame_ms":${n}}`,
            ).frame_ms,
            n,
        );
    }
});
test("required field types, integer limits, unknown tags and malformed syntax reject", () => {
    for (const wire of [
        "",
        "{",
        "[1,]",
        '{"a" 1}',
        "{1:2}",
        "null x",
        "01",
        '"bad\n"',
        '{"seq":1,"state":{}}',
        '{"seq":0,"state":{"state":"unknown"}}',
        frame(1) + "\u00a0",
        ...[
            "-0",
            "-1",
            "1.5",
            "1e2",
            "18446744073709551616",
            '"1"',
            "null",
        ].map((n) => frame(n)),
    ])
        assert.throws(() => decodeFrame(wire));
    for (const wire of [
        '{"state":"ready"}',
        '{"state":"error","message":"x","relink":1}',
        '{"state":"emotion","name":"x","frame_ms":1,"frames":[1]}',
    ])
        assert.throws(() => decodeState(wire));
});
test("strings, escaped punctuation, ignored metadata and safe prototype keys", () => {
    const message = 'écho 🌈 "quoted"\nline \\ 9007199254740993';
    assert.equal(
        decodeState(
            JSON.stringify({ state: "ready", message, metadata: { seq: 2 } }),
        ).message,
        message,
    );
    assert.equal(parseLossless('{"__proto__":7}').__proto__, 7n);
    assert.deepEqual(parseLossless("[[],{},true,false,null,1.5,1e2]"), [
        [],
        Object.create(null),
        true,
        false,
        null,
        1.5,
        100,
    ]);
});
