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
