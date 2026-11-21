import { test } from "node:test";
import assert from "node:assert/strict";
import {
    Lips,
    mouthEvents,
    mouthShape,
    MOUTH_IDLE_MS,
    UNCOVER_MAX_MS,
} from "../../dist/lib/packs/mouth.js";
import { decodeMouth } from "../../dist/lib/decoder.js";
import { hiddenSource, loaded, manifest, packPage, send } from "./page.mjs";

const batch = (sent, samples) =>
    decodeMouth(
        JSON.stringify({
            sent_ms: sent,
            samples: samples.map(([at_ms, open, vowel = "none"]) => ({
                at_ms,
                open,
                vowel,
            })),
        }),
    );
// A manual clock starting at 1000 ms: timers run only as the test advances.
function harness(shapes = ["closed", "half", "open"]) {
    let now = 1000;
    let timers = [];
    const shown = [];
    const schedule = (run, ms) => {
        const timer = { at: now + ms, run };
        timers.push(timer);
        return () => {
            timers = timers.filter((t) => t !== timer);
        };
    };
    const lips = new Lips(
        (shape) => shapes.includes(shape),
        (shape) => shown.push([now, shape]),
        () => now,
        schedule,
    );
    const advance = (ms) => {
        const end = now + ms;
        for (;;) {
            const [next] = timers
                .filter((t) => t.at <= end)
                .sort((a, b) => a.at - b.at);
            if (!next) break;
            timers = timers.filter((t) => t !== next);
            now = next.at;
            next.run();
        }
        now = end;
    };
    return { lips, shown, advance, timers: () => timers.length };
}

test("pack mouth: the wire decodes to numbers and rejects what the contract does not hold", () => {
    assert.deepEqual(batch(1200, [[1400, 62, "a"]]), {
        sent_ms: 1200n,
        samples: [{ at_ms: 1400n, open: 62, vowel: "a" }],
    });
    for (const wire of [
        '{"sent_ms":1}',
        '{"sent_ms":-1,"samples":[]}',
        '{"sent_ms":1,"samples":[{"at_ms":1,"open":256,"vowel":"a"}]}',
        '{"sent_ms":1,"samples":[{"at_ms":1,"open":1.5,"vowel":"a"}]}',
        '{"sent_ms":1,"samples":[{"at_ms":1,"open":1,"vowel":"y"}]}',
        '{"sent_ms":1,"samples":[{"at_ms":1,"open":1}]}',
    ])
        assert.throws(() => decodeMouth(wire), wire);
    assert.equal(
        mouthEvents("http://127.0.0.1:16900/events?token=t"),
        "http://127.0.0.1:16900/events?token=t&mouth=1",
    );
});

test("pack mouth: a vowel still shows for a clear vowel on an open mouth, else openness picks", () => {
    const all = () => true;
    const basic = (shape) => ["closed", "half", "open"].includes(shape);
    for (const [open, vowel, has, expected] of [
        [0, "none", all, "closed"],
        [9, "a", all, "closed"],
        [10, "none", all, "half"],
        [54, "none", all, "half"],
        [55, "none", all, "open"],
        [100, "none", all, "open"],
        // A vowel needs the mouth open far enough to read as one.
        [29, "a", all, "half"],
        [30, "a", all, "a"],
        [80, "u", all, "u"],
        // A pack without that vowel falls back to openness.
        [30, "i", basic, "half"],
        [80, "o", basic, "open"],
        [80, "e", (shape) => shape === "a", "open"],
    ])
        assert.equal(
            mouthShape({ open, vowel }, has),
            expected,
            `${open} ${vowel}`,
        );
});

test("pack mouth: a sample shows when its sound is due, not when it arrives", () => {
    const { lips, shown, advance } = harness();
    lips.state("speaking");
    // Sent at 500 on the bridge's clock, arriving at 1000 on the page's:
    // sound stamped 700 is heard at 1200.
    lips.receive(
        batch(500, [
            [700, 80],
            [720, 30],
            [740, 80],
            [760, 0],
        ]),
    );
    assert.deepEqual(shown, []);
    advance(199);
    assert.deepEqual(shown, []);
    advance(1);
    assert.deepEqual(shown, [[1200, "open"]]);
    advance(60);
    assert.deepEqual(shown.slice(1), [
        [1220, "half"],
        [1240, "open"],
        [1260, "closed"],
    ]);
    // A later batch that sat in a queue for 80 ms keeps its own due times:
    // the quickest batch seen sets the clocks' offset.
    advance(240);
    lips.receive(batch(920, [[1100, 80]]));
    advance(99);
    assert.equal(shown.length, 4);
    advance(1);
    assert.deepEqual(shown.at(-1), [1600, "open"]);
});

test("pack mouth: with no sample for 150 ms the mouth shuts, once", () => {
    const { lips, shown, advance, timers } = harness();
    lips.state("speaking");
    lips.receive(batch(1000, [[1100, 80]]));
    advance(100 + MOUTH_IDLE_MS - 1);
    assert.deepEqual(shown, [[1100, "open"]]);
    advance(1);
    assert.deepEqual(shown, [
        [1100, "open"],
        [1250, "closed"],
    ]);
    advance(5000);
    assert.equal(shown.length, 2);
    assert.equal(timers(), 0);
    // Samples that keep coming hold it open past the fallback.
    lips.receive(
        batch(
            6250,
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9].map((i) => [6300 + i * 100, 80]),
        ),
    );
    advance(1000);
    assert.deepEqual(shown.slice(2), [[6300, "open"]]);
    advance(MOUTH_IDLE_MS);
    assert.deepEqual(shown.at(-1), [7350, "closed"]);
});

test("pack mouth: samples move the mouth only while the pack is speaking", () => {
    const { lips, shown, advance } = harness();
    // Samples lead the speaking state: one already heard is skipped, the
    // next shows on time.
    lips.receive(
        batch(1000, [
            [1050, 80],
            [1100, 30],
            [1300, 80],
        ]),
    );
    advance(110);
    assert.deepEqual(shown, []);
    lips.state("speaking");
    advance(0);
    assert.deepEqual(shown, [[1110, "half"]]);
    advance(190);
    // Nothing was due for 150 ms after the half-open mouth: it shut first.
    assert.deepEqual(shown, [
        [1110, "half"],
        [1260, "closed"],
        [1300, "open"],
    ]);
    // A pause between words: the mouth shuts and stays over the speaking
    // clip while the stage still shows it, replays of its loop included.
    lips.state("wake");
    advance(MOUTH_IDLE_MS);
    assert.deepEqual(shown.at(-1), [1450, "closed"]);
    advance(350);
    lips.revealed("speaking");
    assert.equal(shown.length, 4);
    // The next clip's first frame uncovers it.
    advance(200);
    lips.revealed("wake");
    assert.deepEqual(shown.at(-1), [2000, undefined]);
    // Speech that resumes within the pause keeps the mouth, and a clip
    // revealed while speaking uncovers nothing.
    lips.state("speaking");
    lips.receive(batch(2000, [[2010, 80]]));
    advance(20);
    lips.state("wake");
    advance(400);
    lips.state("speaking");
    lips.revealed("wake");
    advance(5000);
    assert.deepEqual(shown.slice(5), [
        [2010, "open"],
        [2160, "closed"],
    ]);
    // Any other state shuts the mouth at once and drops what is due; the
    // shut mouth covers the speaking clip until the next clip shows.
    lips.receive(batch(7420, [[7430, 80]]));
    advance(10);
    assert.deepEqual(shown.at(-1), [7430, "open"]);
    lips.receive(batch(7430, [[7500, 80]]));
    lips.state("listening");
    assert.deepEqual(shown.at(-1), [7430, "closed"]);
    advance(300);
    assert.equal(shown.length, 9);
    lips.revealed("listening");
    assert.deepEqual(shown.at(-1), [7730, undefined]);
    // A stage that never shows another clip uncovers it in the end.
    lips.state("speaking");
    lips.receive(batch(7730, [[7740, 80]]));
    advance(10);
    lips.state("wake");
    advance(UNCOVER_MAX_MS - 1);
    assert.deepEqual(shown.at(-1), [7890, "closed"]);
    advance(1);
    assert.deepEqual(shown.at(-1), [9740, undefined]);
    // A sample long past is not shown late.
    lips.receive(batch(9740, [[9741, 80]]));
    advance(MOUTH_IDLE_MS + 1);
    lips.state("speaking");
    advance(1000);
    assert.equal(shown.length, 13);
    // Without a mouth showing, a pause has nothing to hold.
    lips.state("wake");
    advance(UNCOVER_MAX_MS);
    assert.equal(shown.length, 13);
});

test("pack mouth: a cut reply shuts the mouth at once and its queued samples never show", () => {
    const { lips, shown, advance } = harness();
    lips.state("speaking");
    // Sent at 1000 for 1100 to 1180; the reply is cut at 1130.
    lips.receive(
        batch(1000, [
            [1100, 60],
            [1120, 60],
            [1140, 60],
            [1160, 20],
            [1180, 60],
        ]),
    );
    advance(130);
    assert.deepEqual(shown, [[1100, "open"]]);
    lips.receive(batch(1130, [[1130, 0]]));
    advance(500);
    assert.deepEqual(shown, [
        [1100, "open"],
        [1130, "closed"],
    ]);
});

test("pack mouth: a restarted sender clock is learnt again", () => {
    const { lips, shown, advance } = harness();
    lips.state("speaking");
    lips.receive(batch(900000, [[900100, 80]]));
    advance(100);
    assert.deepEqual(shown, [[1100, "open"]]);
    // The bridge restarted: its clock is back near zero.
    lips.receive(batch(10, [[60, 30]]));
    advance(50);
    assert.deepEqual(shown.at(-1), [1150, "half"]);
});

const lipSync = (page, samples) =>
    page.evaluate((samples) => {
        // The bridge's clock: this page's, a constant apart.
        const sent = Math.round(performance.now()) + 100000;
        window.streams[0].dispatchEvent(
            new MessageEvent("mouth", {
                data: JSON.stringify({
                    sent_ms: sent,
                    samples: samples.map(([after, open, vowel]) => ({
                        at_ms: sent + after,
                        open,
                        vowel,
                    })),
                }),
            }),
        );
    }, samples);
// 600 ms of one mouth, a sample every 20 ms, from `lead` ms after arrival.
const run = (open, vowel = "none", lead = 0) =>
    Array.from({ length: 30 }, (_, i) => [lead + i * 20, open, vowel]);
const mouth = (page) =>
    page.evaluate(() => {
        const stage = document.querySelector(".pack-stage");
        const still = stage.querySelector(".pack-mouth[data-shown]");
        return {
            lips: stage.hasAttribute("data-lips"),
            stills: stage.querySelectorAll(".pack-mouth").length,
            shown: still
                ? new URL(still.dataset.src).pathname.split("/").at(-1)
                : null,
        };
    });
// The page's clock is paused: time moves only here.
const tick = (page, ms) => page.clock.runFor(ms);
const mouthed = {
    ...manifest,
    character: {
        ...manifest.character,
        mouth: {
            closed: "rest.png",
            half: "mouth-half.png",
            open: "mouth-open.png",
            a: "mouth-a.png",
        },
    },
};

test("pack page: a pack without a mouth set speaks with its clip exactly as before", async (t) => {
    const page = await packPage(t);
    await page.locator(".screen-pack").waitFor();
    // The page asks the screen server for lip sync on its one stream.
    assert.deepEqual(
        await page.evaluate(() =>
            window.streams.map((stream) => new URL(stream.url).search),
        ),
        ["?token=synthetic-screen-pair&mouth=1"],
    );
    await send(page, 1, { state: "ready", message: "Ready" });
    await send(page, 2, { state: "speaking" });
    await lipSync(page, run(90, "a"));
    await tick(page, 100);
    assert.equal(await hiddenSource(page), "speaking.webm");
    await tick(page, 300);
    assert.deepEqual(await mouth(page), {
        lips: false,
        stills: 0,
        shown: null,
    });
    assert.deepEqual(
        await page.evaluate(() =>
            [...document.querySelectorAll(".pack-stage > *")].map(
                (node) => getComputedStyle(node).visibility,
            ),
        ),
        ["visible", "visible", "visible"],
        "two clips and the rest still",
    );
    // A malformed batch changes nothing either.
    await page.evaluate(() =>
        window.streams[0].dispatchEvent(
            new MessageEvent("mouth", { data: "{" }),
        ),
    );
    assert.equal(await hiddenSource(page), "speaking.webm");
});

test("pack page: a mouth set covers the speaking clip with the still for the sample due now", async (t) => {
    const page = await packPage(t, mouthed);
    await page.locator(".screen-pack").waitFor();
    // Every still is decoded and in place before the pack shows; none shows.
    assert.deepEqual(await mouth(page), {
        lips: false,
        stills: 4,
        shown: null,
    });
    // A still shown again is drawn with the frame that hides the clip under
    // it: it decodes with that frame, so the stage is never empty between.
    assert.deepEqual(
        await page.evaluate(() =>
            [...document.querySelectorAll(".pack-mouth")].map(
                (still) => still.decoding,
            ),
        ),
        ["sync", "sync", "sync", "sync"],
    );
    // Samples outside speaking move nothing.
    await send(page, 1, { state: "ready", message: "Ready" });
    await lipSync(page, [[0, 90, "none"]]);
    await tick(page, 200);
    assert.equal((await mouth(page)).lips, false);
    await send(page, 2, { state: "speaking" });
    // A loud run first heard 400 ms after it arrives: nothing on arrival,
    // the open still when its first sample is due.
    await lipSync(page, run(90, "none", 400));
    await tick(page, 399);
    assert.equal((await mouth(page)).lips, false);
    await tick(page, 1);
    assert.deepEqual(await mouth(page), {
        lips: true,
        stills: 4,
        shown: "mouth-open.png",
    });
    // The clip and rest still under it are out of sight, not unloaded.
    const under = await page.evaluate(() =>
        [
            ...document.querySelectorAll(
                ".pack-stage > video, .pack-stage > img:not(.pack-mouth)",
            ),
        ].map((node) => getComputedStyle(node).visibility),
    );
    assert.deepEqual(under, ["hidden", "hidden", "hidden"]);
    assert.equal(
        await page
            .locator(".pack-mouth[data-shown]")
            .evaluate((node) => getComputedStyle(node).visibility),
        "visible",
    );
    // The run's last sample is due 580 ms later; 150 ms after it with no
    // other sample, the closed still, still covering the clip.
    await tick(page, 580 + 149);
    assert.equal((await mouth(page)).shown, "mouth-open.png");
    await tick(page, 1);
    assert.deepEqual(await mouth(page), {
        lips: true,
        stills: 4,
        shown: "rest.png",
    });
    for (const [open, vowel, still] of [
        [30, "none", "mouth-half.png"],
        [70, "a", "mouth-a.png"],
        // The pack has no still for this vowel: openness picks.
        [70, "o", "mouth-open.png"],
    ]) {
        await lipSync(page, run(open, vowel, 20));
        await tick(page, 20);
        assert.equal((await mouth(page)).shown, still);
        await tick(page, 580 + 150);
        assert.equal((await mouth(page)).shown, "rest.png");
    }
    // A pause between words keeps her shut mouth while the speaking clip is
    // held; her own clip shows again with the next clip's first frame.
    await send(page, 3, { state: "connected" });
    await tick(page, 499);
    assert.equal((await mouth(page)).shown, "rest.png");
    await tick(page, 300);
    assert.equal((await mouth(page)).shown, "rest.png");
    await loaded(page);
    assert.equal((await mouth(page)).lips, false);
    // Listening shuts her mouth at once, over the speaking clip, until the
    // listening clip shows.
    await send(page, 4, { state: "speaking" });
    await loaded(page);
    await lipSync(page, run(90, "none", 20));
    await tick(page, 20);
    assert.equal((await mouth(page)).shown, "mouth-open.png");
    await send(page, 5, { state: "listening" });
    assert.equal((await mouth(page)).shown, "rest.png");
    await loaded(page);
    assert.deepEqual(await mouth(page), {
        lips: false,
        stills: 4,
        shown: null,
    });
    await tick(page, 1000);
    assert.equal((await mouth(page)).lips, false);
    // Reduced motion keeps the rest still: no mouth moves.
    await page.emulateMedia({ reducedMotion: "reduce" });
    await send(page, 6, { state: "speaking" });
    await lipSync(page, run(90, "none", 20));
    await tick(page, 100);
    assert.equal((await mouth(page)).lips, false);
});

test("pack page: a mouth set whose still cannot be decoded leaves the speaking clip", async (t) => {
    const page = await packPage(t, mouthed);
    await page.locator(".screen-pack").waitFor();
    assert.equal((await mouth(page)).stills, 4);
    // An optional vowel still that fails is a vowel the pack lacks.
    await page.evaluate(() => {
        window.failDecode = "mouth-a.png";
        const decode = HTMLImageElement.prototype.decode;
        HTMLImageElement.prototype.decode = function () {
            return this.dataset.src.includes(window.failDecode)
                ? Promise.reject(new Error("controlled"))
                : decode.call(this);
        };
    });
    const reload = (revision) =>
        page.evaluate((revision) => {
            window.packRevision = revision;
            window.streams[0].dispatchEvent(
                new MessageEvent("screen-pack-reload", {
                    data: JSON.stringify({ version: 1, revision }),
                }),
            );
        }, revision);
    const reloaded = (revision) =>
        page.waitForFunction(
            (revision) =>
                document
                    .querySelector(".pack-stage img")
                    ?.dataset.src.includes(`revision=${revision}`),
            revision,
        );
    await reload(1);
    await reloaded(1);
    assert.equal((await mouth(page)).stills, 3);
    await send(page, 1, { state: "speaking" });
    await lipSync(page, run(70, "a", 20));
    await tick(page, 20);
    assert.equal((await mouth(page)).shown, "mouth-open.png");
    // A required still that fails leaves the pack without lip sync.
    await page.evaluate(() => {
        window.failDecode = "mouth-half.png";
    });
    await reload(2);
    await reloaded(2);
    await send(page, 2, { state: "speaking" });
    await lipSync(page, run(90, "none", 20));
    await tick(page, 100);
    assert.deepEqual(await mouth(page), {
        lips: false,
        stills: 0,
        shown: null,
    });
});
