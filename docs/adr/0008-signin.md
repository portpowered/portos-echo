---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted
---

# Operator sign-in command library

`echo-signin` hosts writer-based operator commands without depending on the
bridge binary. It uses `echo-config`, `echo-chatgpt-auth`, `thiserror` and
`tokio` already in the workspace; tests reuse `base64` and `serde_json`.
No external package is added to the dependency closure.

The existing auth source remains the sole owner of protocol, validation,
locking, storage and refresh. Its additive `CredentialStatus`/`status` API reads
stored account and expiry metadata through the existing locks and validated
loader, including expired records. Inspection never refreshes or clears an
uncertain refresh marker. Commands expose only the last four account characters
and discard underlying writer errors.

Each resolver constructs one shared auth source and a bounded kill notification.
Construction does not read credentials or sign in. The bridge host owns command
parsing and active call/audio cancellation. The runtime credential adapters and
their receiver-level proof follow in the remaining sign-in story.

The coordinator's binding response authorizes only the metadata addition in
`echo-chatgpt-auth/src/lib.rs` and one new auth status test file. No other auth
implementation or shared contract changes are needed.
