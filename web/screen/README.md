# Echo screen page

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
each message.data must contain one published ScreenFrame JSON object. Only a
strictly newer unsigned 64-bit seq replaces the presentation, including across
reconnections. Reload resets ordering. All supplied strings display as inert
text. Link instructions never navigate automatically.

Actions use fetch with POST, Content-Type application/json and exact UiAction
JSON. Only HTTP 204 reports acceptance; the server-owned presentation stays in
place until a newer frame. Controls are disabled during a request. Rejection,
non-204 responses and the 10-second timeout show “Action could not be sent”.
There is no automatic action retry. Ready sends tap target primary; approval
buttons send their exact id; errors offer Dismiss and optionally Relink.

`npm run generate:schema` derives model types and validation metadata from the
read-only schemas; `npm run check:schema` rejects drift. The decoder preserves
integer tokens as BigInt before validation. Shared contracts require a separate
coordinator-owned change.

Tests load the compiled page in Chromium through pipes and in-memory content,
with controlled EventSource/fetch, fake time and denied browser requests. No
test server is opened. Semantic snapshots and screenshots cover the task-001
presentations. Generated screenshots and coverage stay in ignored test-results.
To intentionally regenerate semantic snapshots use
`UPDATE_SCREEN_SNAPSHOTS=1 npm test` in a POSIX shell. Review changes before
committing. Emotion playback, full pixel regression and complete CI artifact
handoff remain task el-w1-screen-page-002. This is isolated page proof; real
authenticated bridge transport (W2-SCREEN), assembled W1 (W1-INTEGRATION),
Android compatibility/density (S3-WEBVIEW), and device milestones remain unproved.
