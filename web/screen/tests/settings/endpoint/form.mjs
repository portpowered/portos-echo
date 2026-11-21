import { test } from "node:test";
import assert from "node:assert/strict";
import { calls as allCalls } from "../../browser.mjs";
import { response } from "../page.mjs";
import { open, portosStatus, v2Snapshot } from "../page-v2.mjs";

test("paired Port OS endpoint form validates and saves only endpoint fields", async (t) => {
    for (const viewport of [
        { width: 960, height: 600 },
        { width: 1280, height: 774 },
    ]) {
        const initial = v2Snapshot();
        Object.assign(initial.portos, {
            endpoint_name: "Echo Show",
            endpoint_enabled: false,
        });
        const page = await open(t, initial, 200, { context: { viewport } });
        const row = page.getByRole("region", { name: "Port OS", exact: true });
        const name = row.getByLabel("Name in Port OS");
        const enabled = row.getByLabel("Connect this Echo to Port OS");
        const save = row.getByRole("button", { name: "Save Port OS endpoint" });
        await row
            .getByRole("button", { name: "Remove Port OS link" })
            .press("Tab");
        assert.equal(
            await name.evaluate((input) => input === document.activeElement),
            true,
        );
        assert.equal(await name.inputValue(), "Echo Show");
        await name.fill(" \u2003");
        await name.press("Enter");
        await row
            .getByText("Enter 1 to 64 characters without control characters.")
            .waitFor();
        assert.equal(await name.getAttribute("aria-invalid"), "true");
        assert.equal(
            (await allCalls(page)).filter(({ method }) => method === "PATCH")
                .length,
            0,
        );
        await name.fill("Hall Echo");
        await name.press("Tab");
        assert.equal(
            await enabled.evaluate((input) => input === document.activeElement),
            true,
        );
        await enabled.press("Space");
        await enabled.press("Tab");
        await response(page, { version: 2, restart_pending: false });
        await save.press("Enter");
        await page.getByText("Saved Port OS endpoint settings.").waitFor();
        const calls = await allCalls(page);
        assert.deepEqual(
            calls
                .filter(({ method }) => method !== "GET")
                .map(({ method, body }) => ({
                    method,
                    body: JSON.parse(body),
                })),
            [
                {
                    method: "PATCH",
                    body: {
                        version: 2,
                        settings: {
                            portos: {
                                endpoint_name: "Hall Echo",
                                endpoint_enabled: true,
                            },
                        },
                    },
                },
            ],
        );
        await portosStatus(page, {
            endpoint_name: "Hall Echo",
            endpoint_enabled: true,
        });
        await page.evaluate(() => {
            window.settingsV2Body.warnings.push(
                "Approve this Echo's Port OS endpoint connection.",
            );
            window.settingsV2Body.portos.challenge = {
                verification_uri: "https://portos.example/approve",
                user_code: "TEST-CODE",
                expires_at_unix: 1800000000,
            };
        });
        await page.clock.runFor(500);
        await row
            .getByText("Approve this Echo's Port OS endpoint connection.")
            .waitFor();
        await row.getByText(/Code: TEST-CODE/).waitFor();
        await page.evaluate(() => {
            window.settingsV2Body.warnings = [
                "Port OS endpoint connection is unavailable.",
            ];
            delete window.settingsV2Body.portos.challenge;
        });
        await page.clock.runFor(500);
        await row
            .getByText("Port OS endpoint connection is unavailable.")
            .waitFor();
        await page
            .getByRole("button", { name: "Back", exact: true })
            .press("Enter");
        await page
            .getByRole("button", { name: "Settings", exact: true })
            .press("Enter");
        await name.waitFor();
        assert.equal(await name.inputValue(), "Hall Echo");
        assert.equal(await enabled.isChecked(), true);
    }
});
