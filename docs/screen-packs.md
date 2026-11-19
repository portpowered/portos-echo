---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 19
status: in progress; configured installer implemented, selection API and renderer pending
---

# Screen packs

Screen packs are downloadable layout and character data for the bridge's screen page.
The bridge owns their installation and rendering. No pack code or character art belongs
in hassmic or the echo-screen APK.

## Download configuration

`[screen] packs` defaults to an empty list, preserving existing configurations.
Each entry has exactly two required string fields: `url` and `sha256`.
The URL must use HTTPS without inline credentials, whitespace, backslashes or a fragment.
The digest must contain exactly 64 hexadecimal digits; uppercase digits are accepted.
Nonempty downloads require an absolute `[home] dir`.

```toml
[screen]
pairing_token_file = "private/screen-pairing.txt"
packs = []
```

A release pack will use a URL ending in `<name>-<version>.screenpack.tar.gz`.
Its digest pins the archive bytes. A placeholder digest never bypasses verification.
Parsing this configuration performs no downloads and does not open credential
files or create a home. Running the bridge installs configured archives serially
in the background. Each completion records `screen.pack_download` with a safe
installed identity or fixed failure reason in the bridge trace. Settings-page
download status and interactive installation are still pending in this lane.

The installer requires `codex.ca_file` and trusts only those PEM roots. It disables
proxy use, retries and HTTP downgrade, permits at most three HTTPS redirects and
bounds each operation to 60 seconds. Missing or unreadable roots fail the download
without stopping the bridge. A sha256 mismatch is refused before archive parsing
or extracted writes. The compressed download is a temporary file, removed after use.

Installation refuses absolute paths, empty/dot/dotdot components, backslashes,
links, devices, FIFOs, GNU/PAX extensions, sparse entries and duplicate names.
Limits are 65 MiB compressed, 64 MiB expanded regular-file payload, 64 files
including `pack.json`, and 8 MiB per WebM/MP4 clip. Tar headers and padding have
their own bounded decoding budget. Only json/css/png/webp/webm/mp4 files are
accepted. Validation stages files under `screens` before an atomic directory
rename to `<home>/screens/<name>@<version>/`; an existing conflicting identity is
retained. Catalog routes, previews, interactive status and reload events remain
implementation work. Installed data does not yet activate a renderer.

## Authenticated pack files

Once the bridge mounts its home-backed installer (including an empty download
list), `GET /screen-packs/v1/<pack>/<file>?token=<pairing-token>` serves installed
data. `<pack>` is an exact local name or `name@version`; nested file paths use
normalized ASCII components. Percent-encoded path aliases are refused. The same
pairing admission as `/events` applies before disk access and again before a
response is returned; revoked tokens cannot read pack data.

Every read revalidates the whole editable pack, references, CSS and size/file
limits. Invalid packs, identity mismatches, symlinks, hardlinks and special
files return an empty 404. Missing pack hosting and concurrent read saturation
return 503. Validation runs outside the async executor, with one disk reader
per server. Only json/css/png/webp/webm/mp4 are served, with exact MIME types and
`nosniff`, `no-store`, `no-referrer` headers. Single byte ranges support explicit
start/end, open end and suffix forms; malformed, multiple or unsatisfiable
ranges return 416 with `Content-Range: bytes */<total>`.

CSS is served with selectors scoped to `.screen-pack`. Validated image
references are rooted at this pack and acquire the encoded pairing token;
authors never include credentials in their stylesheet. These rewritten bytes
are the representation used for CSS ranges. Page activation is still pending.

## Pack data validation

`pack.json` requires `format: 1`, `name`, `version`, and at least one of `layout`
and `character`. Unknown fields are refused. A character requires a PNG/WebP
`rest` file; states contain `clip` and optional `enter`/`exit` clips, each with
`mode` (`loop` or `once`) and at least one role-matching `webm`/`mp4` file.
All references must resolve to normalized relative files in the same pack.
Future `wake`, `dnd`, `timer` and `call` states validate but have no contract state
yet. Emote names have 1–128 characters. Playback is pending.

Layouts require a percent viewport `safe_area` (`x`, `y`, `width`, `height`),
`slots` (optional `clock`, `date`, `status`, `link_code`, `approval`, `error`) and
`stage` (`anchor: {x,y}`, positive `scale` up to 2, `background: #RRGGBB`).
Areas must fit the viewport. Slots have percent `x`/`y`, optional `color` and
`font_scale` from 0.5 to 2. An optional `css` references an in-pack stylesheet.

The CSS grammar accepts only `.pack-clock`, `.pack-date`, `.pack-status`,
`.pack-link-code`, `.pack-approval`, `.pack-error` and `.pack-stage`, including
comma-separated lists. Allowed declarations are six-digit hex `color`,
`background-color`, `border-color`; `background-image: url(local.png)` (or WebP);
`font-weight` (normal, bold, 400–700 in hundreds); `text-align` (left/center/right);
`opacity` (0–1); and `font-size`, `border-radius`, `border-width`, `padding`, `gap`
(one nonnegative value up to 100 with px/rem/em/%). Unsupported properties,
selectors, comments, escapes, at-rules, expressions and remote/data URLs fail
closed. All CSS files are checked, including unreferenced ones. Serving will
scope selectors and rewrite validated URLs with pairing admission; raw pack CSS
is never part of the built-in page.

## Sparse selection settings

The optional root `screen_pack` key in `<home>/settings.toml` selects a pack:

```toml
screen_pack = "template@0.1.0"

[voice]
session_listen_enabled = true
```

- Omitted: the screen owner will choose an installed template automatically, otherwise
  the built-in page. Resolution against the installed catalog is pending.
- Empty string: explicitly disable packs.
- A bare name: select an exact local name, with `template` reserved for automatic
  template version resolution.
- `name@version`: select an exact installed version.

Names start with a lowercase ASCII letter, contain only lowercase letters, digits and
hyphens, and have at most 48 characters. Versions have three nonempty decimal components
and an optional prerelease made of dot-separated nonempty ASCII alphanumeric components.
Paths, separators, extra `@` characters and build suffixes are rejected.

The bridge's existing settings writer preserves this key across voice or cap changes.
Selection-only persistence does not request a voice restart. Existing voice and cap
changes retain their restart behavior. Invalid settings reject the entire overlay and
retain the validated base with a safe diagnostic. Before rolling back to a bridge with
the older strict settings parser, remove the root `screen_pack` key.
