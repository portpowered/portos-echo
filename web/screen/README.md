# Echo screen page

The production loopback transport is the `echo-screen-server` library. It uses
the same page and endpoint grammar, with `?token=` on each protected endpoint.
For example, percent-encode `/events?token=synthetic-screen-pair` as the `events`
parameter, and `/actions?token=synthetic-screen-pair` as `actions`. Product tokens
are supplied by the bridge; never put them in logs or committed URLs.

Build this page before serving it. The library serves the current bundle from
`web/screen/dist/screen.js`; an absent bundle returns 503, with no fallback.
`serve(loopback_address, Secret<String>, bounded_state_receiver)` returns the
bound address and a bounded action receiver. Frames start at sequence 1; late
clients receive the current frame then new updates. Missing, empty, duplicate,
wrong and revoked tokens receive empty 401 responses. Revocation closes active
streams. Explicit shutdown, owner drop and state input closure close the server.
Backlogs, action queues and active streams have capacity 32; excess requests
receive 503 and lagging streams disconnect. POST accepts JSON up to 65536 bytes
and refuses a supplied foreign Origin. No action is executed or retried here.
LAN access must wait for pinned TLS in W3.

Every served document names its page build in `<meta name="echo-build">`: a
digest of the document, both stylesheets and the script bundle. Each time the
events stream opens, the page fetches the document again and reloads itself
once when the build named there is not its own, so a bridge upgraded in place
never keeps driving the previous page. It reloads at most once per build,
waits while Settings holds unsaved edits, and does nothing when no build is
named.

The optional home-backed settings host adds a fixed
`<meta name="echo-settings" content="v1">` capability to the served page.
That page shows a top-right Settings gear. Back or Escape returns to the
current authoritative state and restores keyboard focus to the gear. The
underlying stream keeps accepting newer frames during editing; navigation
never creates a frame or sends an action. Muted remains visible in Settings.

Settings loads the versioned `/settings/v1` snapshot with the existing action
endpoint's pairing token. Only changed allowlisted fields are submitted;
the voice options come from the bridge, and daily caps remain decimal text
without JavaScript integer rounding. The two prompt editors submit exact
nonblank UTF-8 text of at most 32 KiB to the fixed voice/runner endpoints.
Port OS status and bridge version are read-only; Relink sends the existing
action. Error messages display as inert text and keep the entered values.
All settings requests have a 10-second bound and no automatic retry. A timed
out save can already have committed; reopen Settings to load accepted values
before retrying. Back and unload cancel outstanding requests and timers.
The settings panel scrolls independently while Back and status stay visible
at both 1280x800 and 1280x774. Pages without the capability retain the original
presentation and full-page snapshots.

Settings wears the Port OS look. The page links `tokens.css`, which the screen
server emits from the vendored `portos-tokens` crate with the dark scheme forced;
the Settings rules in `src/screen.css` take every colour, space, radius, border
and font from those custom properties. Manrope is served from this origin at
`fonts/manrope-latin-wght-normal.woff2` (SIL OFL 1.1, `src/fonts/OFL.txt`).
`tools/tokens.mjs` rebuilds the same sheet into `dist/tokens.css` for the page's
tests, and the server integration test compares the two byte for byte. Home,
state pages and screen packs keep their own colours and the system font.

The Echo shows the page at 960x600 CSS px and has no system keyboard. The open
panel has a title row with Back at the top right, in the gear's corner, and the
fields take the rest of the height with the label left of its control. Whole
numbers have − / + buttons clamped to the field's range; holding one repeats.
Focusing a text field or a prompt opens a built-in keyboard (letters, digits,
punctuation, Shift, space, Del, Enter, Done) docked under the fields; it types
at the caret, fires `input` events and never sets `inputmode`, so a real
keyboard still works. Relink's result names the link status: when Port OS is
not linked it says to run `echo-bridge link` on the PC, and it reports
on-screen instructions only when the bridge republishes its relink error.

`npm test` builds the Rust `screen_fixture` example once before running tests.
The integration file consumes that prebuilt artifact, drives synthetic states
through stdin and observes exact returned actions through stdout, using native
EventSource/fetch in locked Chromium on loopback. Run the focused integration
from any working directory using its absolute path after building the page and
fixture. No bridge trace mapping, product pairing persistence or Android behavior
is claimed by this component proof.

Run with Node 24. Provision the locked developer tools separately from tests:

```sh
cd web/screen
npm ci --offline=false
npx --offline playwright install chromium
npm ci --offline
npm test
npm run dev
```

The viewer prints `screen-dev: http://127.0.0.1:4173/?events=%2Fevents&actions=%2Factions`.
It binds only 127.0.0.1:4173, cycles published fixture states, and records at most
32 actions in memory. Restart to clear the queue. It never executes product
actions. SIGINT/SIGTERM closes the stream and listener; an occupied port fails
with `screen-dev: port unavailable`, with no fallback port.

The page entry grammar is
`/?events=percent-encoded-event-url&actions=percent-encoded-action-url`.
Both URLs are required HTTP(S) URLs resolved relative to the page and must have
the same origin. Example: `http://127.0.0.1:4173/?events=%2Fevents&actions=%2Factions`.
Invalid configuration makes no connections or POSTs. The page uses EventSource;
each message.data must contain one published ScreenFrame JSON object. On one
connection only a strictly newer unsigned 64-bit seq replaces the presentation.
A restarted bridge counts from 1 again, so the first frame of each connection
replaces the presentation unless it is the frame already showing. Reload resets
ordering. The browser reconnects a lost stream by itself, but a stream answered
with an error status (a bridge still stopping, a full client table) stays
closed, so the page opens a new one 1 s, 2 s, then every 4 s after such a
closure until one opens. "Screen connection interrupted" shows until then and
clears when the stream opens. All supplied strings display as inert
text. Link instructions never navigate automatically.

Actions use fetch with POST, Content-Type application/json and exact UiAction
JSON. Only HTTP 204 reports acceptance; the server-owned presentation stays in
place until a newer frame. Controls are disabled during a request. Rejection,
non-204 responses and the 10-second timeout show “Action could not be sent”
for 4 seconds; a pending or accepted action shows no status text.
There is no automatic action retry. Ready sends tap target primary; approval
buttons send their exact id; errors offer Dismiss and optionally Relink.
Emotion also offers Dismiss; sending it does not stop playback until the bridge
replaces the presentation. Frames are inert strings advanced at the exact
`frame_ms` duration. Empty animations say “No animation frames”; single-frame
and zero-duration animations stay on their first frame. Large durations use
bounded timer chunks without rounding the u64. Replacement and page unload
cancel animation and pending actions. Long text scrolls within its content area
while controls remain in the viewport; long labels can scroll within a button.

`npm run generate:schema` derives model types and validation metadata from the
read-only schemas; `npm run check:schema` rejects drift. The decoder preserves
integer tokens as BigInt before validation. Shared contracts require a separate
coordinator-owned change.

Tests load the compiled page in Chromium through pipes and in-memory content,
with controlled EventSource/fetch, fake time and denied browser requests. No
test server is opened. The suite covers all 11 states, both Error variants,
exact corner/edge and keyboard actions, scrolling, reconnect ordering and
animation cleanup. Semantic snapshots and platform-specific SHA-256 goldens of
the complete 1280x800 PNG capture detect pixel changes with zero tolerance.
Generated screenshots and the complete test/measurement/coverage transcript
stay in ignored `test-results`; CI uploads them as `screen-evidence-<head>`.
Review inspects Ready, Approval and Emotion in that archive and posts its link.
To intentionally regenerate goldens, run `npm run build`, then
`UPDATE_SCREEN_SNAPSHOTS=1 node --test --test-concurrency=1 tests/*.test.mjs`
in a Linux POSIX shell. Inspect every
changed PNG and semantic snapshot before committing its golden hash; run normal
`npm test` afterward. Linux pixel baselines protect rendering. This is isolated page proof; real
authenticated bridge transport (W2-SCREEN), assembled W1 (W1-INTEGRATION),
Android compatibility/density (S3-WEBVIEW), and device milestones remain unproved.

## Render snapshots

`tests/render-snapshots/` renders every screen state at 960x600 in headless
Chromium, the size of the Echo panel in CSS pixels, so people can look at the
whole screen: the generated `default@0.3.0` face, a full-page avatar fixture
built from the same media over a full-bleed backdrop, and Settings with a
linked, an unlinked and a link-required Port OS account. Requests go to a
synthetic loopback origin answered in process; nothing leaves the browser.

Each render asserts the layout as well: no state text over the face, the clock
and date centred on one line, the gear present, cards only in link code,
approval and error, every surface inside the page and clear of the face and of
each other, round eyes of at least 220 CSS px spanning 60-70% of the width, and
a centroid that moves at most 24 px when idle cuts to Speaking (five runs from
different points of the idle loop).

PNGs land in `test-results/screenshots/render/`, which CI uploads with the
other screen evidence. Each set is also tiled into a half-size contact sheet,
`<set>-sheet.png`, compared with the committed `references/<set>.png`. A pixel
differs when a channel moves by more than 24; a set fails when more than 0.2% of
its pixels differ, and the failure writes `<set>-diff.png` with the differing
pixels in red. References hold generated art only.

`npm test` runs the suite after generating the packs. To run it alone, in a
Linux POSIX shell:

```sh
npm run build
sh ../../scripts/make-template-screenpack.sh --version 0.3.0 --name default --out /tmp/default-pack
SCREEN_PACK_DEFAULT_FIXTURE=/tmp/default-pack node --test --test-concurrency=1 tests/render-snapshots/render.test.mjs
```

To update the references after an intended change, run the same command with
`UPDATE_RENDER_SNAPSHOTS=1`, look at every `*-sheet.png`, then commit the
changed `references/*.png`.
