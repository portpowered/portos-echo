---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 20
status: implemented; downloadable bridge-rendered layouts and characters
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
An installed template activates automatically when selection is omitted.

## Paired controls

Open Settings and choose **Load screen packs**. This explicit request keeps a
pack-hosting failure independent of conversation settings. The panel lists valid
installed identities and their authenticated rest previews (layout-only packs have
no preview). Select **Disable screen packs** or an installed pack,
then **Use screen pack** to save through the existing settings owner. Pack saves
require no restart and update the page without navigation.
While Settings covers the presentation, its catalog and preview reads take
priority. Appearance refreshes coalesce until **Back** exposes the presentation;
the saved selection takes effect then, without another save or reload event.

**Add from URL** requires an HTTPS URL and exactly 64 hexadecimal SHA-256 digits.
**Download pack** queues a background job; it does not wait for the download.
Use **Refresh packs** to read queued/downloading/installed/failed statuses and safe
failure categories. There is no automatic retry or polling. Failed requests keep
entered values; a queued request clears its URL/digest. Back, Escape and unload
cancel the panel's request, and an uncertain response asks the user to reopen and
check before saving again. Pairing failures request reopening the paired screen.

While a request is pending, editing and submission controls are disabled; Back
remains available. Completion restores keyboard focus to the initiating control,
or **Refresh packs** when saving replaces the selection form. Tab moves through
the native pack selector and submission controls. A failed preview displays
**Preview unavailable** without preventing selection. If the saved pack is
unavailable, the panel reports that the built-in appearance is being used.

## Authenticated pack files

Once the bridge mounts its home-backed installer (including an empty download
list), `GET /screen-packs/v1/<pack>/<file>?token=<pairing-token>` serves installed
data. `<pack>` is an exact local name or `name@version`; nested file paths use
normalized ASCII components. Percent-encoded path aliases are refused. The same
pairing admission as `/events` applies before disk access and again before a
response is returned; revoked tokens cannot read pack data.

Every read revalidates the whole editable pack, references, CSS and size/file
limits. Invalid packs, identity mismatches, symlinks, hardlinks and special
files return an empty 404. Missing pack hosting returns 503. Concurrent file,
catalog and selection reads wait in FIFO order for one foreground disk reader per server,
outside the async executor. The existing 128 HTTP/1 connection limit bounds
waiting handlers; no extra reader or retry is created. Disconnect/shutdown
cancels pending admission, and drain refuses queued work. Only
json/css/png/webp/webm/mp4 are served, with exact MIME types and
`nosniff`, `no-store`, `no-referrer` headers. Single byte ranges support explicit
start/end, open end and suffix forms; malformed, multiple or unsatisfiable
ranges return 416 with `Content-Range: bytes */<total>`.

CSS is served with selectors scoped to `.screen-pack`. Validated image
references are rooted at this pack and acquire the encoded pairing token;
authors never include credentials in their stylesheet. These rewritten bytes
are the representation used for CSS ranges.

## Pack data validation

`pack.json` requires `format: 1`, `name`, `version`, and at least one of `layout`
and `character`. Unknown fields are refused. A character requires a PNG/WebP
`rest` file; states contain `clip` and optional `enter`/`exit` clips, each with
`mode` (`loop` or `once`) and at least one role-matching `webm`/`mp4` file.
All references must resolve to normalized relative files in the same pack.
Future `wake`, `dnd`, `timer` and `call` states validate but have no contract state
yet. Emote names have 1–128 characters.

A character may carry a `mouth` set for [lip sync](#lip-sync): PNG/WebP stills
`closed`, `half` and `open`, all three required, and any of the optional vowel
stills `a`, `i`, `u`, `e`, `o`. Each still is the whole character at the size of
the clips, with only the mouth different. Unknown keys are refused. A pack
without a `mouth` set is unchanged.

```json
"mouth": {"closed": "rest.png", "half": "mouth-half.png", "open": "mouth-open.png", "a": "mouth-a.png"}
```

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
`opacity` (0–1); `font-size`, `border-radius`, `border-width`, `padding`, `gap`
(one nonnegative value up to 100 with px/rem/em/%); and `text-shadow` (one or two
comma-separated shadows, each `<x>px <y>px [<blur>px] <colour>` with offsets from
-16 to 16, blur from 0 to 16 and the colour `#RRGGBB` or `rgba(r, g, b, a)`).
Unsupported properties,
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

## Page playback and layout

The paired home-backed page reads the catalog on startup and SSE reconnect,
and refreshes it on `screen-pack-reload`. It derives authenticated same-origin
file URLs from the selected identity, cancels superseded reads, and changes the
look without navigating or changing the ordinary screen-frame sequence. Invalid
or unavailable packs restore the built-in presentation.

Ready and Connected select `idle`; LinkCode selects `setup`. Other current
states select the same lowercase name. Missing states use `idle`, then the rest
still. Unknown Emotion names retain the existing frame animation. Known emotes
play once and resume the prior state loop. State clips with `mode: once` return
to rest after completion. Enter and exit clips play once. Every clip must begin
and end at the same rest pose; loop clips must join seamlessly.

Two muted inline videos share one stage. The old video stays visible while the
next loads; the page reveals the next video on its first decoded frame. Every
state change cuts at once, without waiting for the current clip's end. When the
character's bright pixels sit in a different place in the next clip, the new
video glides from the old position (240 ms) while the old one fades out, so a
cut never jumps. Superseded media callbacks cannot advance newer clips.
VP9-capable browsers choose WebM; otherwise they choose MP4. Reduced-motion
preference applies states at once with no glide.

Connected (the awake gaps of a session) plays the `wake` state when a pack has
one, else stays on `idle`. Speaking follows the bridge, which publishes Speaking
while reply audio plays and Connected in the gaps. The page debounces it in one
place: Speaking starts 100 ms after it arrives, and the mouth keeps moving
through gaps shorter than 500 ms; a longer silence closes it once. Listening,
Error, Approval, Muted and setup end Speaking at once.

## Lip sync

A pack with a `mouth` set moves its mouth with the voice instead of playing a
fixed speaking loop. A pack without one keeps its speaking clip and ignores the
samples.

The bridge measures each 20 ms frame of reply audio as it hands the frame to the
Echo: an openness from 0 to 100 (loudness over a noise floor, opening fast and
closing slower) and a vowel (`a`, `i`, `u`, `e`, `o` or `none`) from the two
strongest resonances of the frame. It is plain arithmetic on the PCM the bridge
already holds: no model, no extra dependency.

Each sample is stamped with the time its sound will be heard: the frame's place
in the Echo's queue plus `[screen] mouth_delay_ms` (40 ms by default, at most
1000). The Echo plays a frame about 85 ms after its place in the queue and the
page draws a still about 35 ms after it is due, so 40 puts the picture about
15 ms ahead of the sound, the side the eye forgives. Samples leave in small
batches, at most five (100 ms), as a named `mouth` event on `/events`:

```text
event: mouth
data: {"sent_ms":1180,"samples":[{"at_ms":1300,"open":70,"vowel":"a"}]}

```

Both times are milliseconds on the bridge's monotonic clock. The page learns the
difference between that clock and its own from the quickest batch it has seen,
and shows each sample when its `at_ms` comes, not when it arrives. The schema is
[`mouth_batch.json`](../schemas/screen/mouth_batch.json).

Only a stream opened with `mouth=1` in its query gets these events; the page asks
for them when the screen server has pack hosting. They never consume
`ScreenFrame.seq` and are never replayed on reconnect. Sending never waits: a
batch goes only to a client with most of its queue free, a busy screen server or
a slow page loses the batch, and audio delivery is not delayed either way. While
no stream has asked, the bridge analyses nothing.

The page decodes every mouth still before the pack shows. While the pack is
speaking, the still for the sample due now covers the speaking clip: under 10
the `closed` still; a vowel still when the sample names a vowel the pack has and
openness is at least 30; otherwise `half`, or `open` from 55. With no sample for
150 ms the mouth shuts. The speaking clip stays on the stage until the next
clip's first frame is ready, so the shut mouth stays over it until then: through
a pause between words, where samples still move it, and after any other state,
which shuts it at once. A stage that shows no other clip uncovers it after 2 s.
Reduced motion shows no mouth stills.

When a reply is cut (a barge-in or a stop word), the bridge drops the samples it
still holds and sends one shut sample stamped now. The page replaces every
queued sample due later with it, so the mouth never moves for sound that was
never played.

Tune `mouth_delay_ms` on the device: raise it when the mouth moves before the
voice, lower it when the mouth trails. Measure it from a recording of the
screen and the sound together, never from the bridge's send times. Each reply's `reply.quality` trace record
carries `lips`: `batches`, `samples`, `dropped` (batches no page took),
`publish_max_ms` (the longest a sample waited from hand-off to being sent),
`lead_min_ms` (the least time a sample was sent ahead of its sound) and `late`
(samples sent at or after their sound).

The page never shows a state name or the Ready message over a pack's face; only
the link code, approval and error cards keep their heading. A known emote is
dismissed by a tap anywhere, with no visible button.

Safe-area coordinates are percentages of the viewport. Slot coordinates are
percentages within that safe area. Clock and date stay on screen in every state;
the clock updates once a second. Their slot `x` is the line their `text-align`
anchors to, so centred text centres on `x`. Card slots right of the stage, or in
the right half over a full-page stage, are fitted to the right edge. Action
feedback floats at the top centre, between the corners, and neither takes
layout space. The stage anchor is its center within the safe area;
scale 1 occupies half its width and height. Text is clamped inside the area,
with oversized cards scrollable and native approval controls retained. Character
only packs keep default positions; layout only packs retain built-in content.
Pack CSS is scoped to this presentation, never to Settings.

## Generate the template and release asset

On Linux with Python 3 and ffmpeg (libvpx-vp9 and libx264 enabled):

```sh
pack_root=$(mktemp -d)
sh scripts/make-template-screenpack.sh --version 0.3.0 --name default --out "$pack_root/generated"
cd "$pack_root/generated"
sha256sum --check default-0.3.0.screenpack.tar.gz.sha256
```

`--name` is `template` (the default) or `default`; it sets the pack name, the
`name@version` directory and the `name-version.screenpack.tar.gz` archive.

The output directory must not exist. Generation publishes it only after every
file and archive succeeds; failure leaves an existing output untouched. No media,
fonts or artwork are downloaded.

Default 0.3.0 is a big friendly face that fills the page. Its stage is the whole
960 × 600 page (anchor 50/50, scale 2) and its eleven silent clips are 960 × 600
at 30 fps, one frame pixel per CSS pixel. Two round eyes, each 240 px across,
span 65% of the width; blinks and lids squash the circle vertically and glances
move it. Idle wanders and blinks with no mouth. Once woken (`wake`, listening,
thinking, speaking) a closed mouth sits below the eyes; only speaking opens it.
Listening widens the eyes in cyan, thinking narrows them and looks up, muted
holds two red slits and error narrows them in red-orange. The link code,
approval and error clips draw a smaller face on the left so the card has the
right 40% of the page. Frames are transparent outside the face; nothing covers
the rest still, so no static eyes show behind a glance. The clock and date share
the page's centre line along the bottom (`pack.css` centres them).

Template 0.2.0 draws two glowing eyes in a
transparent still and ten silent 640 × 400 clips at 30 fps. The 16-second idle
loop looks around and blinks. Listening holds a cyan gaze at center, thinking
looks upward, and speaking animates a small mouth. Muted holds two still red
slits, error narrows the eyes in red-orange, and the `happy` emote lifts the eyes
into arcs before returning to rest. At 960 × 600 the stage occupies 696 × 435
pixels, with the clock and date below and native prompts and controls in a
column to its right. Earlier template versions retain their original art and
layout.

WebM uses lossless VP9 with alpha (`yuva420p`, `-auto-alt-ref 0`); MP4 uses H.264
`yuv420p`, CRF 18 and fast-start metadata. MP4 has no alpha: transparent portions
are rendered against the page and stage color `#101827`. Loop endpoints join in
the state's pose so Listening and Muted hold their expression from the first
frame. The `happy` emote returns to the centered idle pose used by `rest.png`.
Keep any new clips silent and within the size limits above.

`make dist DIST_VERSION=0.3.0` generates a separate
`default-0.3.0.screenpack.tar.gz`, adds its exact digest to `SHA256SUMS`, and pins
that version's release URL and digest in the runtime archive's example config
before computing `MANIFEST`. For source installs, use a published pack URL and
its digest from the same release's `SHA256SUMS`, or set `packs = []`.
The runtime archive keeps the pack media separate. Packaging refuses full generated
payloads embedded in the real bridge binary or the built JavaScript. The release
workflow uploads the default and Companion archives and publishes both on tag releases;
workers use manual workflow runs, never create release tags.

For a local installation without device access, place the generated
`template@0.2.0` directory under a fresh bridge home's `screens/` directory, then
start the bridge with that home and open its paired page. With both versions
installed, omitted selection activates 0.2.0. Select `template@0.1.0` or
**Disable screen packs** to roll back without deleting either pack or restarting
the bridge. A download test must serve the generated archive from a
local HTTPS peer trusted exclusively by the configured CA; never use public
release downloads in tests.

## Companion pack

Companion 0.2.2 is an optional cel-shaded character with poses for Echo's screen
states and a happy emote. In Listening, she cups an ear with her mouth closed.
Her `mouth` set drives [lip sync](#lip-sync): the neutral frame is the closed
mouth, `mouth-half.png` and `mouth-open.png` the other two, and each vowel
drawing under `scripts/packs/companion/art/` is published when it exists.
The clock and date share a centre line below the character. Codex image generation
produced an original reference sheet and edited it into the poses. The checked-in PNGs and their
[generation prompts](../scripts/packs/companion/prompts.md) are the source for
the offline video build.

On Linux with Python 3 and ffmpeg (libvpx-vp9 and libx264 enabled):

```sh
pack_root=$(mktemp -d)
sh scripts/make-companion-screenpack.sh --version 0.2.2 --out "$pack_root/generated"
cd "$pack_root/generated"
sha256sum --check companion-0.2.2.screenpack.tar.gz.sha256
```

For a released build, use the HTTPS URL for
`companion-0.2.2.screenpack.tar.gz` and that file's digest from the same
release's `SHA256SUMS`. Enter both in **Settings > Load screen packs**. After
installation, select **companion 0.2.2**, choose **Use screen pack**, then
choose **Back** to see the new appearance. The release example pins the release's
default pack. To roll back, select the installed default pack or **Disable screen packs**,
save with **Use screen pack**, and choose **Back**.

## Local push

On a Linux authoring host with Python 3 and adb:

```sh
sh scripts/echo-screen-pack.sh push "$pack_root/generated/template@0.2.0"
```

Physical device operations belong to the coordinator's authorized T5 procedure.
Worker tests put a recording fake adb on PATH; they never contact a device. The
helper uses adb's existing device selection. It targets the installed bridge
home only:
`/data/local/echo-bridge/home/screens/<name>@<version>/`. A custom bridge home
requires copying validated local data into that home's `screens/` directory
through the operator's own file procedure.

Before any adb call, the helper snapshots bounded regular files and checks the
format-1 schema, references, layout, file caps and the same restricted CSS grammar
documented above. Links, special files and executables are refused. The source
directory must be stable while it is read; transfers consume the private snapshot.
The bridge independently revalidates local data when discovering or serving it.

A private hidden directory under `screens/` holds transfers. An exclusive hidden
lock refuses concurrent helper runs. The staged directory has the final pack
identity as its basename, so toybox `mv -n` can publish it into `screens/`
without nesting it inside a competing pack. The helper refuses existing
identities, including a competing publication during transfer. Use a new version
for replacement. Direct active-file editing remains available through the settled
watcher described above.
Directories are private (0700), files are 0600, and ownership matches the installed
bridge (9999:9999). Symlinked destination ancestors are refused. Transfer and
publication failures remove this helper's staging and lock when adb remains
reachable. If cleanup fails, inspect the final identity, hidden `.push-*` entry
and lock before removing this run's leftovers and retrying. A cleanup failure
after publication may leave a complete pack installed. Leave another helper's
lock in place.

Success prints `screen-pack: pushed <name>@<version>`. An adb failure names the
step and reports a recognized reason from stderr.
For example, `adb transfer failed: Permission denied`.
Other stderr is redacted to protect serial numbers and paths. Cleanup errors
appear separately after any original failure. Push does not write settings or
restart the bridge. Select the installed identity from the paired Settings page. Omitted
selection automatically adopts an installed template; an explicitly empty
selection keeps the built-in appearance. Select **Disable screen packs** to roll
back without deleting downloaded data.

## Device-free verification

On Linux with the repository's pinned Rust/Node tools, Python 3, OpenSSL, ffmpeg
(VP9 and H.264 encoders), and the pinned Playwright Chromium already provisioned:

```sh
cd web/screen
npm ci --offline
npm test
```

The suite owner builds the screen fixtures and runs the generation command above
once in a fresh temporary directory. The integration journey consumes those
prebuilt artifacts, a fresh home and a local HTTPS archive peer. It follows the
paired controls above: download with the generated digest, refresh, disable,
select the template, refuse a wrong digest, observe a touched clip, and roll back
to **Disable screen packs**. It checks the atomic selection file and native SSE,
fetch and decoded video frames; only the incoming voice states and external
archive peer are controlled. The production settings service reports whether a
restart was requested when the fixture shuts down.

The existing release-helper test entry point exercises the generation/checksum
and local-push instructions with a recording fake adb:

```sh
python3 tests/test_release_kit.py
```

These procedures never contact a device or account. Generated data and temporary
homes are removed after the suite. Hosted Linux evidence belongs in the PR;
independent release-artifact and clean-room checks remain review-owned. Chromium
decoding does not establish physical WebView behavior: device playback remains
coordinator-owned T5 proof.
