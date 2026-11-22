# Direct Muse gadget service

`MuseLink` owns one prepared gadget pairing and registers it over the Muse daemon
stream. Its Linux caller supplies the path to the private pairing record and the
Mozilla PEM roots shipped with Codex. The startup sequence validates the pairing
and roots, selects a VM lease over HTTPS, opens the authenticated WebSocket, and
completes Noise XX. `wait_registered` reports readiness after a matching
`link.register` acknowledgement. `stop` releases the link and pairing lease.

The version 1 pairing JSON contains `node_id`, `access_token`, `refresh_token`,
`access_token_saved_at` (Unix seconds), and `noise_static_key` (32 base64-encoded
bytes). `api_url_v2` and `noise_host` may override the pinned protocol defaults.
The file must be a regular, singly linked file owned by the process user, with
mode 0600 in an owner-controlled directory. A sibling lock holds exclusive
ownership for the lifetime of the service.

The service selects the first usable default VM or falls back to the first
usable lease. A WebSocket upgrade rejected with HTTP 401 or 403 gets one fresh
lease lookup and one replacement upgrade within that attempt. Failed attempts
back off before starting a new lookup.

At a persisted access-token age of three hours, the service releases the old
link, atomically saves both replacement tokens and their timestamp, and
registers again. An incomplete or uncertain refresh ends that instance and
preserves the previously committed pairing. A refresh rejected with HTTP 401
removes the pairing and leaves the service in `PairingRemoved`.

The control link runs up to four report invocations at a time. If all are
busy, waiting requests start in arrival order and excess requests receive an
error. Control result output is limited to 96 KiB after JSON escaping, within
the 256 KiB control frame limit. This limit leaves the accepted task summary
intact.

Either `link.unpaired` or `node.unpaired` closes the connection, discards
in-flight and waiting invocations, and removes the private pairing. A removal
event in the final control-stream body takes effect before the stream closes. The
instance stays in `PairingRemoved` and refuses submissions. If removal fails,
`stop` reports a storage error. Operator stop also releases the connection
and discards invocation work, and keeps the pairing available for a new
instance. Pending tasks keep their original deadlines.

`MuseLink::start` also takes a separate caller-owned task snapshot path. The
service commits each task ID, side-chat UUID and absolute deadline before it
sends the prompt. Assistant text from the acknowledged side chat yields
`Progress`. A matching `echo.report.result` commits the only remote terminal
outcome. The absolute deadline produces a nonretryable failure if the report
never arrives. Cancelling commits the local cancellation before advising the
original chat. A report grants no user consent.

Reopening the owner-only snapshot retains pending events and task tombstones.
An uncertain send or interrupted subscription leaves the task eligible for
its original report or deadline. The service sends each task prompt once.

The crate is exercised through its local Rust boundary and real loopback HTTPS,
WebSocket and encrypted daemon framing. Use the component suite on Linux:

```sh
cargo test --locked -p echo-svc-muse --test noise_vectors -- --nocapture
cargo nextest run --locked -p echo-svc-muse --all-targets --config-file .github/nextest.toml --profile ci
```

## Attribution

NOTICE: This crate's wire compatibility and trust checks reference Meta
Platforms, Inc. and affiliates' Apache-2.0 [Muse Gadget SDK](https://github.com/facebookincubator/muse-gadget-sdk/tree/ded4222aaa6eaef5ac99257f52a29bd53235e562).
The Noise fixture is the exact protocol match from the
[Cacophony vector corpus](https://github.com/haskell-cryptography/cacophony/blob/8ee9d41e34a1a596cfa3ab12aa4069ff87dc1247/vectors/cacophony.txt),
with its [Unlicense notice](tests/fixtures/UNLICENSE).
