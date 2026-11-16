---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted
---

# Operator sign-in command library

`echo-signin` hosts writer-based operator commands without depending on the
bridge binary. It uses `echo-config`, `echo-chatgpt-auth`, `thiserror` and
`tokio` already in the workspace. Runtime adapters use `echo-live`,
`echo-agent-loop`, `echo-contracts` and `reqwest`; tests use `echo-agent-api`. All
are existing workspace dependencies; tests reuse `base64` and `serde_json`.
No external package is added to the dependency closure.

The existing auth source remains the sole owner of protocol, validation,
locking, storage and refresh. Its additive `CredentialStatus`/`status` API reads
stored account and expiry metadata through the existing locks and validated
loader, including expired records. Inspection never refreshes or clears an
uncertain refresh marker. Commands expose only the last four account characters
and discard underlying writer errors.

Each resolver constructs one shared auth source and a bounded kill notification.
Construction does not read credentials or sign in. The bridge host owns command
parsing and active call/audio cancellation. The runtime adapters share this auth
source for serialized refresh. API-key mode
reads only the configured file and strips one terminal LF/CRLF. Both existing
consumer traits map failures to their safe authorization categories. A separate
bounded watch notification invalidates pending acquisitions on every transition
to on, retaining cancellation even across an immediate on/off pulse. A final
publication check prevents a completed acquisition from escaping cancellation.
The public boolean watch remains the host's signal for active resources.

The coordinator's binding response authorizes only the metadata addition in
`echo-chatgpt-auth/src/lib.rs` and one new auth status test file. No other auth
implementation or shared contract changes are needed.
