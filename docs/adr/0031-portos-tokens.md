---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 20
status: accepted
---

# Vendored Port OS design tokens

The owner asked for the Echo Settings screen to look like the Port OS UI: the Manrope face and the same primary
colours, spacing, margins, borders and radii. Port OS publishes its design tokens as a generated Rust crate,
`portos-tokens` 0.1.0, in the public repository `https://github.com/portpowered/portos-ui-rust` under the
Apache License 2.0, the same licence as this repository. It is `no_std`, has no dependencies and is not
published to crates.io.

`third_party/portos-tokens` holds that crate's manifest, licence, README and source byte for byte.
`echo-screen-server` is its
only dependent, by path. This adds one package to `Cargo.lock` and no registry package. The screen server emits
the crate's `CSS` constant at `/tokens.css`, followed by the dark scheme's values repeated on `:root`, and the
Settings rules in `web/screen/src/screen.css` read those custom properties. No token value is written by hand in
this repository.

Port OS follows `prefers-color-scheme`. The Echo forces the dark scheme: the panel sits in a dark room, the home
screen and the screen packs are dark, and a WebView's reported scheme is not under our control.

The tokens name one face, Manrope. `web/screen/src/fonts` holds `manrope-latin-wght-normal.woff2` and its licence
text from `@fontsource-variable/manrope` 5.3.0 (SIL Open Font License 1.1). The screen server embeds the font and
serves it at `/fonts/manrope-latin-wght-normal.woff2`; the page loads nothing from another origin.

`scripts/sync-portos-tokens.sh <rev> [<font-checkout>]` fetches a tag or commit of that repository into a
temporary directory, copies the four files and, when given a Port OS monorepo checkout, the font from its
installed package. It writes `third_party/portos-tokens/vendor.sha256` with the repository, the full commit and
each file's SHA-256. `scripts/sync-portos-tokens.sh --check` runs in `make lint-self-tests`
and fails when a vendored file differs from the manifest. Vendored files are replaced by the sync, never edited.

The crate is generated code with a 651-line source file and a name outside the `echo-` set, so `lint-shape`
treats `third_party/portos-tokens/` as a vendored root, as it does `factory/`. `cargo-deny` reads the crate's
Apache-2.0 licence like any other dependency's.

Cargo makes a path dependency inside the workspace directory a workspace member. The generated manifest has no
`[lints]` table and is never edited here, so Cargo prints its `missing_lints_inheritance` warning for that one
manifest. The warning fails nothing. Excluding the crate from the workspace instead breaks `cargo fmt --all` in a
worktree nested under another checkout, and a workspace lint level does not reach a manifest that inherits none.
The clean fix is upstream: the generator can emit an empty `[lints]` table.

Rollback removes the path dependency, the two routes and the vendored directories, and restores the literal
colours in `screen.css`.
