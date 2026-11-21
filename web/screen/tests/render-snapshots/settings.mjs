// Synthetic Settings data for the render snapshots; no real account.
import { readFile } from "node:fs/promises";

// The dark scheme of the vendored Port OS tokens, read from the crate itself:
// `token("PRIMARY")` is the CSS colour Settings must compute to.
const crate = await readFile(
    new URL(
        "../../../../third_party/portos-tokens/src/lib.rs",
        import.meta.url,
    ),
    "utf8",
);
const dark = crate.split("pub mod dark {")[1].split("\n    }\n")[0];
export function token(name) {
    const hex = new RegExp(
        `pub const ${name}: Color = Color::new\\(0x[0-9A-F]{8}, "#([0-9a-f]{6})"\\);`,
    ).exec(dark)[1];
    const channels = [0, 2, 4].map((at) => parseInt(hex.slice(at, at + 2), 16));
    return `rgb(${channels.join(", ")})`;
}

// What Settings computes to and whether it fits the viewport: the first font
// family of each kind of text, the token-driven colours and radii, and every
// visible control that crosses a viewport edge.
export function settingsLook(page) {
    return page.evaluate(() => {
        const panel = document.querySelector(".settings-panel");
        const style = (selector) =>
            getComputedStyle(
                selector === "main"
                    ? document.querySelector("main")
                    : panel.querySelector(selector),
            );
        const first = (selector) =>
            style(selector).fontFamily.split(",")[0].trim();
        const save = ".settings-section[open] .settings-actions > button";
        const outside = [
            ...panel.querySelectorAll(
                "h1, h2, h3, summary, label, button, input, select, textarea",
            ),
        ]
            .filter((node) => node.checkVisibility())
            .map((node) => [node, node.getBoundingClientRect()])
            .filter(
                ([, box]) =>
                    box.width > 0 &&
                    (box.left < 0 || box.right > window.innerWidth),
            )
            .map(([node]) => node.tagName + " " + node.textContent.trim());
        const box = panel.getBoundingClientRect();
        const fields = panel
            .querySelector(".settings-fields")
            .getBoundingClientRect();
        return {
            families: [
                ...new Set(
                    ["h1", "h2", "summary", "label", "input", save].map(first),
                ),
            ],
            manrope: [...document.fonts]
                .filter((face) => face.family === "Manrope")
                .map((face) => `${face.weight} ${face.status}`),
            background: style("main").backgroundColor,
            foreground: style("h1").color,
            primary: style(save).backgroundColor,
            primaryText: style(save).color,
            saveRadius: style(save).borderTopLeftRadius,
            inputBorder: style("input:not([type=checkbox])").borderTopColor,
            inputRadius: style("input:not([type=checkbox])")
                .borderTopLeftRadius,
            outside,
            panel: [box.left, box.top, box.right, box.bottom],
            fields: [fields.left, fields.top, fields.right, fields.bottom],
            page: [
                document.documentElement.scrollWidth,
                document.documentElement.scrollHeight,
            ],
        };
    });
}

const scopes = ["endpoint:read", "endpoint:write", "message:send"];
const tools = ["control", "endpoint_query", "endpoint_show", "guide"];

export function settingsBody(portos) {
    return {
        body: {
            version: 2,
            settings: {
                voice: {
                    session_listen_enabled: true,
                    session_listen_seconds: 15,
                },
                codex: {
                    voice: "marin",
                    web_search: "cached",
                    delegate_model: "synthetic-runner",
                    delegate_effort: "low",
                    delegate_fast: false,
                },
                agent: {
                    daily_request_cap: "1000",
                },
                home: { agent_tools: ["notes", "prompts"] },
            },
            allowed_voices: ["default", "marin", "cedar"],
            prompts: {
                voice: "Voice instructions",
                runner: "Runner instructions",
            },
            bridge_version: "0.1.0",
            restart_pending: false,
            warnings: [],
            portos: {
                link_status: portos,
                operation_state: "idle",
                link_waiting_for_restart: false,
                scope_choices: scopes,
                tool_choices: tools,
                server: {
                    name: "portos",
                    url: "https://127.0.0.1/mcp",
                    requested_scopes: ["endpoint:read", "message:send"],
                    granted_scopes:
                        portos === "linked"
                            ? ["endpoint:read", "message:send"]
                            : [],
                    allow_tools: ["endpoint_query", "control"],
                },
            },
        },
        openAI: {
            version: 2,
            link_status: portos === "linked" ? "linked" : "link_required",
            operation_state: "idle",
        },
    };
}
