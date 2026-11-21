// The token stylesheet the screen server emits at /tokens.css, rebuilt from
// the vendored crate so the page's tests load the same custom properties.
// crates/echo-screen-server/src/screen_assets.rs is the product source;
// tests/integration/server.test.mjs compares the two byte for byte.
import { readFile } from "node:fs/promises";

const crate = new URL(
    "../../../third_party/portos-tokens/src/lib.rs",
    import.meta.url,
);
const DARK_OPEN = "@media (prefers-color-scheme: dark) {\n  :root {\n";
const DARK_CLOSE = "  }\n}\n";

export async function tokensCss() {
    const source = await readFile(crate, "utf8");
    const css = /pub const CSS: &str = r##"([^]*?)"##;/.exec(source)?.[1];
    if (!css) throw new Error("portos-tokens: CSS constant not found");
    const dark = css.split(DARK_OPEN)[1]?.split(DARK_CLOSE)[0];
    if (!dark) throw new Error("portos-tokens: dark scheme not found");
    return `${css}\n/* Echo: always the dark scheme. */\n:root {\n${dark}}\n`;
}
