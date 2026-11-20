---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 19
status: in progress; installer, catalog and selection persistence implemented; hot load and renderer pending
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
installed identity or fixed failure reason in the bridge trace. Configured and
interactive downloads share one serial worker. The catalog exposes safe job
status, available through Settings > Load screen packs.

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
retained. Paired installation is available from the settings panel.
Installed data does not yet activate a renderer.

## Paired controls

Open Settings and choose **Load screen packs**. This explicit request keeps a
pack-hosting failure independent of conversation settings. The panel lists valid
installed identities and their authenticated rest previews (layout-only packs have
no preview). Select **Built-in appearance** to disable packs or an installed pack,
then **Use screen pack** to save through the existing settings owner. Pack saves
require no restart. The renderer remains pending in this lane.

**Add from URL** requires an HTTPS URL and exactly 64 hexadecimal SHA-256 digits.
**Download pack** queues a background job; it does not wait for the download.
Use **Refresh packs** to read queued/downloading/installed/failed statuses and safe
failure categories. There is no automatic retry or polling. Failed requests keep
entered values; a queued request clears its URL/digest. Back, Escape and unload
cancel the panel's request, and an uncertain response asks the user to reopen and
check before saving again. Pairing failures request reopening the paired screen.

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

- Omitted: the screen owner chooses an installed template automatically, otherwise
  the built-in page.
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

## Catalog and selection API

`GET /screen-packs/v1?token=<pairing-token>` lists only validated packs. Hidden
staging directories and digest metadata are omitted. Bad manifests, mismatched
directory identities, unsafe files and missing references are omitted too.
Entries contain `id`, `name`, `version`, `manifest_url` and `rest_url`; URLs are
relative to the screen server and contain no token. Append the pairing token
when requesting a manifest or preview. A layout-only pack has `rest_url: null`.

The snapshot's `active` is resolved from the existing settings owner's accepted
overlay. Omitted selection defaults to `template`; automatic template selection
chooses the highest installed version, comparing release components numerically,
then prerelease identifiers (numeric before alphabetic, stable after prerelease).
Equal versions use the directory identity as a deterministic lexical tie break.
Other bare names select only an exact local directory. Invalid persisted choices
return `active: null` and `warning: "invalid_selection"`; an empty selection has
no warning. Catalog reads revalidate local data without restarting the bridge.

`POST /screen-packs/v1/selection?token=<pairing-token>` accepts JSON:

```json
{"version": 1, "screen_pack": "template"}
```

An empty `screen_pack` selects the built-in look. Unknown identities return 422
with `{"version":1,"reason":"invalid_selection"}` and preserve the accepted
choice. Valid choices use the same atomic 0600 settings writer as voice settings,
preserving sparse siblings and existing pending voice restart status. Selection
alone never requests a voice restart. Persistence errors use the existing fixed
settings diagnostic and retain the prior accepted choice on pre-rename failure.

Both routes require pairing admission and refuse a supplied foreign Origin.
Selection requires `application/json`, a version-1 object with exactly the two
fields above, and a body of at most 65536 bytes. Revocation, cancellation and
shutdown are checked again before the settings host commits a request. A missing
pack root or settings host returns 503.

## Interactive installation

`POST /screen-packs/v1/install?token=<pairing-token>` accepts an
`application/json` object with exactly `version: 1`, `url` and `sha256`.
The URL must be HTTPS without userinfo or fragment; the digest must contain
exactly 64 hexadecimal digits. Invalid inputs return 422 with a fixed
`invalid_url` or `invalid_sha256` reason, without creating a job. Authentication,
Origin, body size and version checks match selection; admission rechecks
revocation and shutdown before queueing work.

A 202 response contains `{"version":1,"job":"1"}`. Poll the catalog's `jobs`
array: each entry contains only `id`, `status` and `reason`. Status is `queued`,
`downloading`, `installed` or `failed`; reason is null or a fixed category such
as `checksum_mismatch`, `tls_failed` or `download_timeout`. Submitted URLs,
digests and local paths never appear in job status. IDs increase within the
server lifetime; history retains at most 32 entries, keeping unfinished work.
At most four interactive jobs may be unfinished at once; saturation returns
503 without creating a job. Configured pins run first on the same worker.
Jobs are transient, with no automatic retry or selection write. A failed job
keeps installed data and the current selection intact. Shutdown cancels active
work, joins validation and discards queued requests.

## Reload notifications

A successful effective selection change publishes one named
`screen-pack-reload` event on the existing authenticated `/events` stream:

```text
event: screen-pack-reload
data: {"version":1,"revision":1,"active":"template@0.1.0"}

```

`active` is the resolved installed identity or null for the built-in look.
Revision increases independently of `ScreenFrame.seq` and is also returned by
catalog snapshots. Saving the same resolved identity (including changing from
`template` to its exact selected version) publishes nothing. Invalid choices,
pre-rename persistence failures and rejected admission leave revision unchanged.
Mounting or reading the catalog emits no event. Reconnecting returns only the
latest ordinary screen frame; fetch the catalog again for current pack state.
Pairing revocation closes both kinds of publication. Slow clients share the
existing bounded queue and are disconnected rather than blocking voice state.

Each newly published configured or interactive installation also emits one
reload, resolved against the settings owner's accepted selection. This includes
installing a pack while the built-in look is selected (`active: null`). The
completion notification precedes the job's `installed` status. Reinstalling
the same pinned identity and failed installations emit nothing. Notifications
use the existing bounded host inbox and recheck revocation and shutdown before
commit; they neither save settings nor request a voice restart.

The home-backed pack owner also watches `settings.toml`: two matching bounded
reads, 250 ms apart, settle an external edit or removal. It accepts at most
64 KiB of regular UTF-8 data and refuses symlinks and special files. The existing
settings owner validates the complete overlay and checks the read's file identity
again at commit, so a newer HTTP save supersedes a stale read. Overload retries
on a later poll rather than creating a queue. Invalid files retain accepted state
and are reconsidered when the file changes. File removal restores overlay defaults.
An effective pack change emits one reload; unchanged selections and the watcher's
observation of an HTTP save emit none. The watcher does not write settings.

Other externally edited values remain pending for the next normal startup;
they appear in the editable settings snapshot but never change running voice
policy or request a restart. A subsequent screen save preserves those accepted
sibling values. Existing restart requests and durability uncertainty remain intact.

The same lifetime also polls validated pack files off the runtime thread, using
that bounded file reader. Two matching observations settle content and file
metadata, so a touch or atomic replacement with identical bytes counts as an edit.
Only changes to the resolved active pack emit a reload; inactive edits do not.
An invalid active pack falls back to the built-in look and restoration reloads it.
Selection, installation and settled file observation share publication identity,
so an installation seen by the watcher emits one event. Shutdown joins reads;
revocation and draining still refuse final publication. Watching never writes
settings or requests a restart.

The page renderer is still pending. Selecting data does not yet change the main
page's look.
