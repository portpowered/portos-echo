import { test } from "node:test";
import assert from "node:assert/strict";
import { calls } from "../browser.mjs";
import { snapshot } from "../support/snapshot.mjs";
import { response } from "./page.mjs";
import {
    base,
    empty,
    load,
    refresh,
    saveResponse,
    settingsPage,
    status,
    token,
} from "./pack-fixture.mjs";

const pack = (name) => ({
    id: `${name}@1.0.0`,
    name,
    version: "1.0.0",
    rest_url: null,
});
const installed = ["template", "studio", "layout"].map(pack);
const catalog = (packs, active) => ({ ...empty, installed: packs, active });
const button = (page, name) => page.getByRole("button", { name, exact: true });
const select = (page) => page.getByLabel("Screen pack", { exact: true });
const save = (page) => button(page, "Use screen pack");
const rail = (page) =>
    page.getByRole("group", { name: "Installed screen packs" });
const selected = (page) => rail(page).getByRole("button", { pressed: true });
const focused = (control) =>
    control.evaluate((node) => node === document.activeElement);
const contained = (page) =>
    page.evaluate(() => ({
        page: document.documentElement.scrollWidth,
        settings: document.querySelector(".settings-fields").scrollWidth,
    }));

async function pickState(page, value, label) {
    assert.equal(await select(page).inputValue(), value);
    assert.deepEqual(
        await selected(page).evaluateAll((cards) =>
            cards.map((card) => card.getAttribute("aria-label")),
        ),
        label ? [label] : [],
    );
}

function selectionPost(value) {
    return {
        url: base + "/selection" + token,
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ version: 1, screen_pack: value }),
    };
}
const catalogGet = {
    url: base + token,
    method: "GET",
    headers: undefined,
    body: undefined,
};
const newCalls = async (page, from) => (await calls(page)).slice(from);

async function layout(page) {
    return rail(page).evaluate((node) => {
        const { left, top, right, bottom, width } =
            node.getBoundingClientRect();
        const cards = [...node.querySelectorAll("button")].map((card) => {
            const { left, top, right, bottom } = card.getBoundingClientRect();
            return { left, top, right, bottom };
        });
        const css = getComputedStyle(node);
        return {
            left,
            top,
            right,
            bottom,
            width,
            cards,
            overflow: css.overflowX,
            snap: css.scrollSnapType,
            offset: node.scrollLeft,
        };
    });
}
const centered = (row, card) =>
    Math.abs((card.left + card.right - row.left - row.right) / 2) <= 1;

test("packs: carousel snapshot shows three previews and both neighbors of the active pack", async (t) => {
    const page = await settingsPage(t);
    await page.evaluate(() => {
        const imageSrc = Object.getOwnPropertyDescriptor(
            HTMLImageElement.prototype,
            "src",
        );
        Object.defineProperty(HTMLImageElement.prototype, "src", {
            set(url) {
                const canvas = document.createElement("canvas");
                canvas.width = canvas.height = 160;
                const context = canvas.getContext("2d");
                context.fillStyle = url.includes("/template@")
                    ? "#ffdb70"
                    : url.includes("/studio@")
                      ? "#92dbd0"
                      : "#c4d5ff";
                context.beginPath();
                context.arc(80, 80, 59, 0, 2 * Math.PI);
                context.fill();
                imageSrc.set.call(this, canvas.toDataURL());
            },
        });
    });
    const previews = installed.map((item) => ({
        ...item,
        rest_url: `/screen-packs/v1/${item.id}/rest.png`,
    }));
    await load(
        page,
        catalog([previews[1], previews[0], previews[2]], previews[1].id),
    );
    await page
        .locator(".settings-packs")
        .evaluate((node) => node.scrollIntoView({ block: "start" }));
    for (const item of previews) {
        const card = button(page, `${item.name} 1.0.0`);
        assert.equal(
            await card
                .getByText(`${item.name} 1.0.0`, { exact: true })
                .isVisible(),
            true,
        );
        const image = card.getByRole("img", { name: `${item.name} rest pose` });
        assert.equal(
            await image.evaluate(async (node) => {
                await node.decode();
                return node.naturalWidth;
            }),
            160,
        );
    }
    const row = await layout(page);
    const [left, middle, right] = row.cards;
    assert.deepEqual(
        row.cards.map((card) => [card.top, card.bottom]),
        Array(3).fill([middle.top, middle.bottom]),
    );
    assert.ok(
        middle.left >= row.left &&
            middle.right <= row.right &&
            centered(row, middle),
        JSON.stringify(row),
    );
    assert.ok(
        left.left < row.left &&
            row.left < left.right &&
            left.right < middle.left,
        JSON.stringify(row),
    );
    assert.ok(
        middle.right < right.left &&
            right.left < row.right &&
            row.right < right.right,
        JSON.stringify(row),
    );
    assert.equal(row.overflow, "auto");
    assert.equal(row.snap, "x mandatory");
    await pickState(page, previews[1].id, "studio 1.0.0");
    assert.equal(
        await selected(page)
            .getByText("✓ Selected", { exact: true })
            .isVisible(),
        true,
    );
    const hash = await snapshot(page, "packs-carousel-1280x800");
    console.log(
        `carousel snapshot: 1280x800, three cards, active centered; neighbor peeks ${Math.round(left.right - row.left)}/${Math.round(row.right - right.left)} px; sha256 ${hash}`,
    );
});

test("packs: Next stops at the end and stages the exact authenticated save", async (t) => {
    const page = await settingsPage(t);
    const packs = [installed[0], installed[2]];
    const [first, last] = packs;
    const voice = page.getByLabel("voice.md", { exact: true });
    await voice.fill("Keep the unsaved voice edit");
    await load(page, catalog(packs, first.id));
    const from = (await calls(page)).length;
    const previous = button(page, "Previous");
    const next = button(page, "Next");
    assert.equal(await previous.isDisabled(), true);
    await previous.click({ force: true });
    await pickState(page, first.id, "template 1.0.0");
    await next.click();
    await pickState(page, last.id, "layout 1.0.0");
    assert.equal(await next.isDisabled(), true);
    await next.click({ force: true });
    await pickState(page, last.id, "layout 1.0.0");
    assert.deepEqual(await newCalls(page, from), []);
    await saveResponse(page, 200, catalog(packs, last.id));
    await save(page).click();
    assert.match(await status(page).textContent(), /^Screen pack saved\./);
    assert.deepEqual(await newCalls(page, from), [
        selectionPost(last.id),
        catalogGet,
    ]);
    await pickState(page, last.id, "layout 1.0.0");
    assert.equal(await voice.inputValue(), "Keep the unsaved voice edit");
    console.log(
        `carousel save: zero staging writes, one POST ${last.id}, one catalog GET, voice draft retained`,
    );
});

test("packs: a ten-pack catalog reveals its last active card without moving Settings", async (t) => {
    const page = await settingsPage(t);
    const packs = Array.from({ length: 10 }, (_, index) =>
        pack(`pack-${index}`),
    );
    const fields = page.locator(".settings-fields");
    await refresh(page).evaluate((node) =>
        node.scrollIntoView({ block: "center" }),
    );
    await refresh(page).focus();
    await response(page, null, "pending");
    await refresh(page).click();
    assert.equal(await status(page).textContent(), "Loading screen packs…");
    const scroll = await fields.evaluate((node) => node.scrollTop);
    await page.evaluate(
        (data) => window.resolvePost({ status: 200, json: async () => data }),
        catalog(packs, packs[9].id),
    );
    await page.getByText("Screen packs loaded", { exact: true }).waitFor();
    assert.equal(await fields.evaluate((node) => node.scrollTop), scroll);
    assert.equal(await focused(refresh(page)), true);
    const row = await layout(page);
    assert.equal(row.cards.length, 10);
    assert.ok(centered(row, row.cards[9]), JSON.stringify(row));
    assert.deepEqual(await contained(page), { page: 1280, settings: 1280 });
    assert.equal(
        await select(page)
            .getByRole("option", { name: "Default", exact: true })
            .isDisabled(),
        true,
    );
    await button(page, "Previous").click();
    await pickState(page, packs[8].id, "pack-8 1.0.0");
    await button(page, "Next").click();
    await pickState(page, packs[9].id, "pack-9 1.0.0");
    assert.deepEqual((await calls(page)).slice(2), [catalogGet]);
    console.log(
        `carousel catalog: 10 cards, last centered at offset ${row.offset}, Settings scroll stayed at ${scroll}`,
    );
});

test("packs: Default, Disable and legacy template keep their exact save values", async (t) => {
    const page = await settingsPage(t);
    const [automatic, other] = installed;
    const packs = [automatic, other];
    await load(page, catalog(packs, other.id));
    let from = (await calls(page)).length;
    for (const [draft, active, label] of [
        ["default", automatic.id, "template 1.0.0"],
        ["", null, null],
    ]) {
        await select(page).selectOption(draft);
        await pickState(page, draft, null);
        assert.deepEqual(await newCalls(page, from), []);
        await saveResponse(page, 200, catalog(packs, active));
        await save(page).click();
        assert.deepEqual(await newCalls(page, from), [
            selectionPost(draft),
            catalogGet,
        ]);
        await pickState(page, active ?? "", label);
        from += 2;
    }
    const legacy = { ...automatic, id: "template" };
    const response = catalog([legacy, other], legacy.id);
    await load(page, response);
    assert.deepEqual(
        await select(page)
            .locator("option")
            .evaluateAll((options) => options.map((option) => option.value)),
        ["default", other.id, ""],
    );
    await pickState(page, "default", "template 1.0.0");
    await button(page, "template 1.0.0").click();
    assert.deepEqual(await newCalls(page, from + 1), []);
    await saveResponse(page, 200, response);
    await save(page).click();
    assert.deepEqual(await newCalls(page, from + 1), [
        selectionPost("default"),
        catalogGet,
    ]);
    await pickState(page, "default", "template 1.0.0");
    console.log(
        'carousel aliases: POST "default", "", and "default" for the legacy template',
    );
});

test("packs: refused writes and failed reloads preserve the last valid render until refresh", async (t) => {
    const page = await settingsPage(t);
    const [first, last] = installed;
    const packs = [first, last];
    await load(page, catalog(packs, first.id));
    await button(page, "Next").click();
    const from = (await calls(page)).length;
    await response(page, { message: "internal failure details" }, 422);
    await save(page).click();
    assert.match(
        await status(page).textContent(),
        /^Screen pack request was refused/,
    );
    assert.deepEqual(await newCalls(page, from), [selectionPost(last.id)]);
    await pickState(page, last.id, "studio 1.0.0");
    assert.equal(await focused(save(page)), true);

    await saveResponse(page, 503, null);
    await save(page).click();
    assert.equal(
        await status(page).textContent(),
        "Screen packs are unavailable or busy. Refresh packs to try again.",
    );
    await pickState(page, last.id, "studio 1.0.0");
    await page.clock.runFor(10000);
    assert.deepEqual(await newCalls(page, from), [
        selectionPost(last.id),
        selectionPost(last.id),
        catalogGet,
    ]);
    await response(page, { ...empty, revision: -1 });
    await refresh(page).click();
    assert.equal(
        await status(page).textContent(),
        "Pack response could not be read",
    );
    await pickState(page, last.id, "studio 1.0.0");
    await load(page, catalog(packs, first.id));
    await pickState(page, first.id, "template 1.0.0");
    assert.deepEqual(await newCalls(page, from + 3), [catalogGet, catalogGet]);
    console.log(
        "carousel failure: refused POST had no GET; accepted POST with 503 reload had no success or retry; explicit refresh restored authoritative selection",
    );
});

async function swipeAndTap(page) {
    const session = await page.context().newCDPSession(page);
    await session.send("Emulation.setTouchEmulationEnabled", { enabled: true });
    await rail(page).evaluate((node) =>
        node.scrollIntoView({ block: "center" }),
    );
    const row = await layout(page);
    await rail(page).evaluate((node) => {
        window.packSnap = new Promise((resolve) =>
            node.addEventListener("scrollend", resolve, { once: true }),
        );
    });
    const y = Math.round((row.bottom + row.top) / 2);
    const contact = (type, x) =>
        session.send("Input.dispatchTouchEvent", {
            type,
            touchPoints: x === undefined ? [] : [{ x: Math.round(x), y }],
        });
    await contact("touchStart", row.left + row.width * 0.9);
    for (const position of [0.82, 0.74, 0.66, 0.58, 0.5, 0.42, 0.35])
        await contact("touchMove", row.left + row.width * position);
    await contact("touchEnd");
    await rail(page).evaluate(() => window.packSnap, undefined, {
        timeout: 5000,
    });
    const swiped = await layout(page);
    assert.ok(
        swiped.offset > row.offset && centered(swiped, swiped.cards[2]),
        JSON.stringify(swiped),
    );
    await pickState(page, installed[1].id, "studio 1.0.0");
    await contact(
        "touchStart",
        (swiped.cards[2].left + swiped.cards[2].right) / 2,
    );
    await contact("touchEnd");
    await session.detach();
    await pickState(page, installed[2].id, "layout 1.0.0");
    return swiped.offset - row.offset;
}

for (const viewport of [
    { width: 1280, height: 800 },
    { width: 960, height: 600 },
]) {
    test(
        `packs: carousel browser interaction at ${viewport.width}x${viewport.height}`,
        { timeout: 20000 },
        async (t) => {
            const page = await settingsPage(t);
            await page.setViewportSize(viewport);
            await load(page, catalog(installed, installed[1].id));
            const from = (await calls(page)).length;
            await page.keyboard.press("Tab");
            assert.equal(await focused(button(page, "studio 1.0.0")), true);
            await page.keyboard.press("ArrowRight");
            await pickState(page, installed[2].id, "layout 1.0.0");
            const last = button(page, "layout 1.0.0");
            assert.deepEqual(
                await last.evaluate((node) => {
                    const css = getComputedStyle(node);
                    return {
                        focused: node.matches(":focus-visible"),
                        outline: css.outlineWidth,
                        inset: css.outlineOffset,
                    };
                }),
                { focused: true, outline: "4px", inset: "-6px" },
            );
            const row = await layout(page);
            assert.ok(centered(row, row.cards[2]), JSON.stringify(row));
            await page.screenshot({
                path: `test-results/screenshots/packs-carousel-focus-${viewport.width}x${viewport.height}.png`,
            });
            await page.keyboard.press("ArrowLeft");
            await pickState(page, installed[1].id, "studio 1.0.0");
            assert.equal(await focused(button(page, "studio 1.0.0")), true);
            for (const control of [
                button(page, "Previous"),
                button(page, "Next"),
                select(page),
                save(page),
            ]) {
                await page.keyboard.press("Tab");
                assert.equal(await focused(control), true);
                const box = await control.boundingBox();
                assert.ok(
                    box.width >= 64 && box.height >= 64,
                    JSON.stringify(box),
                );
            }
            const swiped = await swipeAndTap(page);
            assert.deepEqual(await contained(page), {
                page: viewport.width,
                settings: viewport.width,
            });
            assert.deepEqual(await newCalls(page, from), []);
            await saveResponse(page, 200, catalog(installed, installed[2].id));
            await save(page).click();
            assert.deepEqual(await newCalls(page, from), [
                selectionPost(installed[2].id),
                catalogGet,
            ]);
            await pickState(page, installed[2].id, "layout 1.0.0");
            assert.equal(
                await selected(page)
                    .getByText("✓ Selected", { exact: true })
                    .isVisible(),
                true,
            );
            for (const control of [button(page, "Back"), status(page)]) {
                const box = await control.boundingBox();
                assert.ok(
                    box.y >= 0 && box.y + box.height <= viewport.height,
                    JSON.stringify(box),
                );
            }
            console.log(
                `carousel browser ${viewport.width}x${viewport.height}: native swipe moved ${swiped} px; tap selected layout@1.0.0; visible keyboard ring and >=64px controls; Save sent one POST`,
            );
        },
    );
}
