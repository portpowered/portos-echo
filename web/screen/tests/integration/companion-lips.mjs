import assert from "node:assert/strict";

const shown = (page) =>
    page.evaluate(() => {
        const stage = document.querySelector(".pack-stage");
        const still = stage.querySelector(".pack-mouth[data-shown]");
        return {
            lips: stage.hasAttribute("data-lips"),
            file: still ? new URL(still.src).pathname.split("/").at(-1) : null,
            visible: still ? getComputedStyle(still).visibility : null,
            size: still ? [still.naturalWidth, still.naturalHeight] : null,
            under: [
                ...stage.querySelectorAll(
                    ":scope > video, :scope > img:not(.pack-mouth)",
                ),
            ].map((node) => getComputedStyle(node).visibility),
        };
    });

const shows = (page, file) =>
    page.waitForFunction((file) => {
        const still = document.querySelector(".pack-mouth[data-shown]");
        return (
            (still ? new URL(still.src).pathname.split("/").at(-1) : null) ===
            file
        );
    }, file);

// The real screen server carries lip-sync batches to the installed Companion
// pack: a loud sample shows her open mouth, a quieter one the half-open mouth.
export async function checkCompanionLips(f) {
    const { page } = f;
    const stills = await page.evaluate(() =>
        [...document.querySelectorAll(".pack-stage .pack-mouth")].map((still) =>
            new URL(still.src).pathname.split("/").at(-1),
        ),
    );
    for (const file of ["rest.png", "mouth-half.png", "mouth-open.png"])
        assert.ok(stills.includes(file), `${file} in ${stills}`);
    const rest = await page.evaluate(() => {
        const still = document.querySelector(".pack-stage .pack-mouth");
        return [still.naturalWidth, still.naturalHeight];
    });
    await f.update({ state: "speaking" });
    // The bridge's clock: this process's. Each run is 600 ms of one mouth, a
    // sample every 20 ms, first heard 300 ms after it is sent.
    const run = (open, vowel = "none") => {
        const sent = Math.round(performance.now());
        return {
            sent_ms: sent,
            samples: Array.from({ length: 30 }, (_, i) => ({
                at_ms: sent + 300 + i * 20,
                open,
                vowel,
            })),
        };
    };
    const sent = Date.now();
    assert.equal(await f.mouth(run(90)), 1);
    assert.equal((await shown(page)).lips, false);
    await shows(page, "mouth-open.png");
    const waited = Date.now() - sent;
    assert.ok(waited >= 290, `open mouth after ${waited} ms`);
    const open = await shown(page);
    assert.equal(open.lips, true);
    assert.equal(open.visible, "visible");
    assert.deepEqual(open.size, rest);
    assert.ok(open.under.length >= 2);
    assert.ok(open.under.every((visibility) => visibility === "hidden"));
    // The voice stops: her mouth shuts over the speaking clip.
    await shows(page, "rest.png");
    assert.equal(await f.mouth(run(30)), 1);
    await shows(page, "mouth-half.png");
    assert.deepEqual((await shown(page)).size, rest);
    await shows(page, "rest.png");
    if (stills.includes("mouth-a.png")) {
        assert.equal(await f.mouth(run(70, "a")), 1);
        await shows(page, "mouth-a.png");
    }
    // Listening shows her own clip again at once.
    await f.update({ state: "listening" });
    assert.deepEqual(await shown(page), {
        lips: false,
        file: null,
        visible: null,
        size: null,
        under: (await shown(page)).under,
    });
    assert.ok(
        (await shown(page)).under.some(
            (visibility) => visibility === "visible",
        ),
    );
    assert.deepEqual(f.outside, []);
    console.log(
        `LIPS stills=${stills.length} open_after_ms=${waited} half=shown closed=shown`,
    );
}
