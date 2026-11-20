import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { settingsPage as basePage, response } from "./page.mjs";

export async function settingsPage(t) {
    let page;
    await mkdir("test-results/screenshots", { recursive: true });
    t.after(async () => {
        if (!page) return;
        await page.screenshot({
            path: `test-results/screenshots/${t.name.replace(/[^a-z0-9]/gi, "-")}.png`,
        });
    });
    page = await basePage(t);
    page.setDefaultTimeout(5000);
    return page;
}

export const empty = {
    version: 1,
    revision: 0,
    active: null,
    warning: null,
    installed: [],
    jobs: [],
};
export const base = "http://127.0.0.1:4173/screen-packs/v1";
export const token = "?token=synthetic-screen-pair";
export const status = (page) => page.locator(".settings-panel [role=status]");
export const refresh = (page) =>
    page.getByRole("button", { name: /^(Load screen packs|Refresh packs)$/ });
export async function load(page, data = empty) {
    await response(page, data);
    await refresh(page).click();
    assert.equal(await status(page).textContent(), "Screen packs loaded");
}

export async function saveResponse(page, nextStatus, nextCatalog) {
    await page.evaluate(
        ({ nextStatus, nextCatalog }) => {
            const fetch = window.fetch;
            window.fetch = (url, options) => {
                const isWrite = options.method === "POST";
                window.postStatus = isWrite ? 200 : nextStatus;
                window.responseBody = isWrite
                    ? { version: 1, restart_pending: true }
                    : nextCatalog;
                if (!isWrite) window.fetch = fetch;
                return fetch(url, options);
            };
        },
        { nextStatus, nextCatalog },
    );
}
