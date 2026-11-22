---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 22
status: proposed; M1 may start (the owner approved `x25519-dalek`); section 6 questions 3 and 4 are open
---

# The Echo as a Muse gadget

Owner (2026-11-22): "we are the muse gadget, that hooks into another muse agent. we're supposed to subsume the muse
gadget responsibilities so that we can talk to the muse directly".

Today the Echo does not reach Muse itself. `echo-bridge` calls `echo-muse-daemon` (crate `echo-muse-bridge`) on a
separate Linux host over HTTPS. That daemon shells out to the Muse gadget SDK's `musegadget send-user-msg`, and Muse
calls back through one command spec, `echo.report.result` ([plan section 7](../program/plan.md#7-muse-a-wrapper-around-the-gadget-sdk)).
This design makes `echo-bridge` on the Echo the gadget: it pairs, registers its commands and talks to the Muse
agent directly. The Linux host, `echo-muse-daemon`, `echo-muse-report` and the `musegadget` service go away.

## 1. What the gadget service does

From `facebookincubator/muse-gadget-sdk` at commit `ded4222` (2026-11-21), Apache-2.0, read only; none of its code
was run. The service is Python (`linux/src/musegadget/`) with `cryptography` and `websockets` as its only
dependencies. There is no full wire specification. The Python service, its tests (including a fake VM that speaks
real Noise) and a second implementation for ESP32 in C are the reference.

| Layer | What it is | Upstream source |
| --- | --- | --- |
| Pairing | A BLE GATT peripheral that the Muse phone app connects to. P-256 ECDH, HKDF-SHA256 and AES-256-GCM, confirmed in the app. The app hands over access and refresh tokens, the Noise host and the API URL. These are saved with a random node id | `ble_server.py`, `ble_setup.py`, `pairing.py`, `identity.py` |
| API | HTTPS to the Muse API: fetch the VM list (WebSocket URL, VM token, default flag) and refresh the device token. Access tokens last about 4 h and are rotated at 3 h; refresh tokens rotate | `muse_api.py`, `service.py` |
| Transport | A TLS WebSocket to the VM's `/v1/noise` endpoint with the VM token, then a `Noise_XX_25519_AESGCM_SHA256` handshake with the gadget as initiator. A 401 or 403 means fetch fresh VM credentials | `link_client.py`, `noise/noise_xx.py` |
| Streams | Protobuf frames (chunked, at most 65489 payload bytes each) that carry HTTP-like request, response, body and reset messages to a service named `daemon` | `noise/framing.py`, `noise/envelope.py`, `noise/transport.py` |
| Control | One long-lived `POST /link-control` stream of JSON messages, each with a little-endian u32 length, at most 256 KiB | `link_client.py` |
| Commands | `link.register` with the node id, display name, platform, `device_family: "gadget"`, model and the catalogue (`commands_v2`). Muse sends `link.invoke {id, command, params, timeout_ms}`; the gadget answers `link.result {id, ok, payload or error}`. At most four run at once; output is capped at 96 KiB. `link.unpaired` means Muse removed the gadget | `commands.py`, `executor.py`, `protocols/DEVICE.md` |
| Chat | `POST /chat/stream` sends a user message (`message`, `device_id`, `output_modality`, optional `session_id`; a new session id is a side chat). `POST /chat/subscribe` returns NDJSON events for the turn | `muse_turn.py` |
| Trust | The system CA store, refused when it still holds either of two compromised roots (exit 78) | `tls.py`, `cli.py` |

## 2. The target

`echo-bridge` gains one service crate, `echo-svc-muse`, in the sense of the
[single-binary plan](2026-11-22-single-binary-plan.md) section 2.4: a service, not a subsystem, with its own
crate, talking to the core loop only through `echo-core-api`. It owns the Muse pairing state, the connection, the
command catalogue and the tasks. The bridge's `ask_muse` voice tool stays as it is; only what sits behind it
changes.

- **To Muse.** `ask_muse` becomes a `chat/stream` message in a side chat per task, followed through
  `chat/subscribe`. This replaces `send-user-msg --wait`. The task rules of plan section 7 stay: a `task_id`, the
  remaining run time as the deadline, progress from interim replies, and an honest failure at the deadline.
- **From Muse.** `echo.report.result {task_id, status, summary}` is registered in the catalogue and handled inside
  the bridge. Completion still comes only from a matching report; chat text never counts as success and a report
  never counts as user consent.
- **Commands we expose.** Only `echo.*` commands of ours. We register none of the SDK's built-ins (`system.run`,
  `file.read`, `file.write`), as plan section 7 already requires. `device.health` and `device.guide` are
  candidates once the owner says what Muse may see.
- **Trust.** Fire OS 7's CA store is old, so the bridge ships its own roots (the same ones the Codex client uses)
  and keeps upstream's refusal of the two compromised roots. The Noise handshake does not authenticate the
  server's static key, so server identity rests on TLS alone; we do not weaken it.
- **Secrets.** The pairing tokens, node id and Noise keys live under `private/muse/` with owner-only modes, like
  the Port OS grant. Only one holder may use a pairing at a time, because refresh tokens rotate.

## 3. Pairing

Muse's phone app pairs only over BLE. Two ways to get a pairing onto the Echo:

| Option | How | For | Against |
| --- | --- | --- | --- |
| **P1 BLE on the Echo** | The screen app runs a GATT server (Android `BluetoothGattServer` and `BluetoothLeAdvertiser`); the bridge does the key exchange. **Settings > Accounts > Muse > Pair** turns Bluetooth on for the pairing only | One device, the quick start stays one flow | Fire OS 7 reports `android.hardware.bluetooth_le`, but peripheral mode is unverified (Bluetooth is off on our Echo, 2026-11-22). Needs Bluetooth permissions in the APK |
| **P2 Pair elsewhere, import** | Pair once with upstream's own service on any Linux machine, then `echo-setup muse import` pushes the pairing state to the Echo and the Linux copy must stop | No BLE work; can ship first | A second machine once; the user must stop the other copy |

Recommendation: build P2 first, because it unblocks everything else. Then do P1 if a T5 probe shows that Fire OS 7
advertises as a peripheral.

## 4. Building it

The protocol is roughly three to four thousand lines of Python; we need no voice, video, BLE (for P2) or wake
pieces. In Rust on armv7:

- `ring` (already locked) gives AES-GCM, SHA-256 and HKDF. `ring` cannot do X25519 with a stored static key,
  which Noise XX needs, so the Noise layer needs **one new crate, `x25519-dalek`**. The XX handshake itself is small
  and is written in-house against upstream's test vectors. P1 also needs P-256 ECDH; `p256` is already in
  Cargo.lock.
- `tokio-tungstenite` and `rustls` (already locked) give the TLS WebSocket.
- The protobuf frames are hand-encoded, as upstream does; no `prost`.

## 5. Phases

| Phase | Delivers | Acceptance (observable) | Deletes |
| --- | --- | --- | --- |
| **M1** Protocol core | `echo-svc-muse`: Noise XX, frames, streams, control, registration, invoke and result, chat send and follow, all against a fake Muse VM in T3 (our own, modelled on upstream's test) | Upstream's Noise test vectors pass; T3: register, an `echo.report.result` invoke completes a task, a 401 refetches VM credentials, a deadline fails honestly, a fifth concurrent invoke waits | Nothing |
| **M2** Import and switch | `echo-setup muse import` (P2); `ask_muse` uses `echo-svc-muse`; Settings shows Muse paired and connected | T5: a harmless Port OS read that Muse performs and reports, ids correlated end to end (the M7 proof), with no Linux host | `crates/echo-muse-bridge`, `echo-muse-daemon`, `echo-muse-report`, the `[muse]` URL, CA and credential settings |
| **M3** Pair on the Echo (optional) | P1 | T5: pair from the Muse app with only the Echo | Nothing |

M1 can run alongside track A of the single-binary plan: it touches no device code. M2 changes the Echo, so it
waits until A1 has passed its T5.

## 6. Owner answers and open questions

1. **The new crate.** Answered (owner, 2026-11-22): "approved overall for dalek". `x25519-dalek` is the one new
   dependency for Noise.
2. **Terms and token.** The owner has the SDK token from the Muse gadgets portal and keeps it outside this
   repository. It is read only at pairing time, by path, and never printed, logged or committed.
3. **Pairing order.** Open. Recommended: P2 first, then P1 if the BLE probe passes. M1 does not depend on it.
4. **What Muse may call.** Open. Until answered, the catalogue has only `echo.report.result`. M1 does not depend on
   it.
