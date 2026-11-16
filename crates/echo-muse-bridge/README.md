# Muse service

This opt-in Linux service hosts the existing correlated Muse backend. It does
not install the SDK, a command spec, a service unit, or an echo-bridge adapter.
A report conveys a result and never grants consent. Real SDK compatibility,
bridge integration and M7 require later authorized validation.

Build and verify from a Linux worktree:

```sh
cargo build -p echo-muse-bridge --bins --locked
cargo test -p echo-muse-bridge --locked
cargo test -p echo-muse-bridge --locked --test service_t3 -- --nocapture
```

Cargo builds the daemon, report handler and Rust fake gadget before the tests.
Component T3 tests use only loopback, disposable generated TLS trust and client
credentials, private temporary storage and an explicitly controlled clock.
Windows runs the portable core and verifies both service executables exit 1
with `Muse service requires Unix`. There is no browser surface.

## Operator configuration

Provision private files outside git; do not paste credentials into commands,
URLs, logs or traces. Use an owner-only directory (0700), regular owner-only
files (0600), and absolute paths without symlinks. The operator supplies the
trusted issuer/certificate hostname, PKCS#8 DER private key, and a matching DER
X.509 certificate. TLS is mandatory even on loopback. Clients verify the issuer
and hostname; a numeric address needs a matching IP subject alternative name.

The client credential file holds 32 through 256 ASCII non-whitespace bytes,
with one optional final LF. The daemon loads the credential and key privately;
they are never arguments. Rotating either requires a clean stop and explicit
restart with provisioned files. Revoking client access means stopping the
listener and replacing the credential before restart.

Example configuration (illustrative paths, no supplied credentials):

```json
{
  "listen": "127.0.0.1:7448",
  "tls_certificate_der": "/private/echo/server.der",
  "tls_private_key_der": "/private/echo/server-key.der",
  "client_credential_file": "/private/echo/client.credential",
  "store": "/private/echo/tasks.json",
  "report_socket": "/private/echo/report.sock",
  "gadget_program": "/private/echo/musegadget",
  "clock": { "source": "system" }
}
```

There is no default address, socket or program. Explicitly choosing a LAN
address exposes only the authenticated HTTPS task API. The operator owns
firewall policy and certificate trust; this code changes neither. Validate
the configuration against [the schema](service-config.schema.json).

```sh
target/debug/echo-muse-daemon --config /private/echo/daemon.json
```

Readiness is one line of JSON with `ready:true` and the bound numeric `listen`
address, emitted only after TLS, store lease, private socket and listener are
ready. Startup errors exit 1 without readiness. Diagnostic output contains
stable codes rather than request, credential, key or gadget diagnostic data.

## Authenticated API

The full wire contract and error table are in
[service.openapi.yaml](service.openapi.yaml). Every request uses one
`Authorization: Bearer` header; authentication precedes routing, body decoding,
task lookup and mutation. Duplicate or malformed credentials return 401 with
exactly `{"error":"unauthorized"}`. Responses use `Cache-Control: no-store`.
There are no public report routes or redirects, and each connection serves
one HTTP/1.1 request.

This Python example reads the credential into memory, verifies the configured
CA and hostname, and prints only task/event data. Save it outside the
repository; keep shell tracing disabled. The example makes actual requests
only when the operator runs it against their explicitly configured service.

```python
import json
import ssl
import urllib.request
from pathlib import Path

base = "https://localhost:7448"
credential = Path("/private/echo/client.credential").read_text().removesuffix("\n")
trust = ssl.create_default_context(cafile="/private/echo/issuer.pem")

def request(method, path, payload=None):
    data = None if payload is None else json.dumps(payload).encode()
    headers = {"Authorization": "Bearer " + credential}
    if data is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(base + path, data, headers, method=method)
    with urllib.request.urlopen(req, context=trust, timeout=6) as response:
        body = response.read(1048577)
        if len(body) > 1048576:
            raise RuntimeError("response too large")
        return None if response.status == 204 else json.loads(body)

task = request("POST", "/v1/tasks", {
    "delegation_id": "delegation-test", "instructions": "Read light",
    "context": {"target": "test-light"},
    "deadline_seconds": 60, "max_tool_calls": 8
})
task_id = task["task_id"]
print(request("GET", f"/v1/tasks/{task_id}/events"))
# Subsequent polls consume Progress and exactly one correlated terminal event.
# To cancel instead, call request("POST", f"/v1/tasks/{task_id}/cancel").
```

Submit returns 201 after durable admission and the initial bounded gadget
request. Conversational replies, including exit 3 partial replies and text
claiming completion, are Progress. Only a matching live report completes the
task. Gadget failures and deadline expiry produce one nonretryable Failed.
Event polls wait at most one second; 204 means there is no event, not success.
Cancellation commits and clears the queue before its advisory original-chat
message; an advisory failure does not undo cancellation. Repeated cancellation
preserves the core behavior.

Events are dequeued durably before transmission. Delivery is at most once:
disconnecting can lose the response even though consumption committed. An
interrupted submit, cancel or report may also have committed. There is no replay
cursor, automatic retry, or exactly-once delivery guarantee.

## Private report handler and ownership

The SDK runner's environment must supply the explicit socket path. The existing
`report_command_spec` remains byte-identical; this PR installs nothing.

```sh
ECHO_MUSE_REPORT_SOCKET=/private/echo/report.sock \
  target/debug/echo-muse-report < /private/echo/report.json
```

Stdin is one UTF-8 report with `task_id`, `status` (`completed` or `failed`)
and `summary`, at most 65536 bytes including whitespace. Success writes the
existing accepted JSON with no newline and empty stderr, then exits 0.
Rejection writes no stdout, the exact core/transport reason on stderr, and
exits 1. It never retries. The handler watchdog exits within eight seconds
even with stalled stdin or stdout. IPC reads and writes have two-second bounds.

IPC is one big-endian u32 length followed by JSON, with a required write
half-close before mutation. The version-1 response envelope contains exactly
`version` and one of `output`/`error`. The handler validates accepted identity,
shape and version before writing stdout. The socket is 0600 inside a 0700
directory. It never opens a store or a second backend.

One daemon holds an OS lease on the canonical store's companion `.lock` file.
The lock inode is retained across shutdown and process death; never unlink it
to bypass ownership. A second owner fails before exposing a listener. Version-1
snapshots and tombstones remain unchanged; writes use private same-directory
temporary files, atomic replacement, file sync and parent-directory sync.
A storage error latches service health failed until restart: HTTP returns
503 `storage_unavailable`, never a misleading empty event or successful
acknowledgement. Preserve the snapshot for recovery; do not delete tombstones.

## Bounds, shutdown and rollback

The service permits 32 combined HTTP/private connections and four concurrent
gadget operations. Excess connections close before TLS when needed; excess
authenticated gadget work returns 503 `service_busy` before admission. Headers
are bounded to 8192 bytes, requests to 65536, responses to 1048576, each gadget
output stream to 98304, and the store to 16777216 bytes / 4096 records.
TLS/header/body/write timers are two seconds, operations four seconds, and
gadget children two seconds with kill and wait on timeout/output failure.
Unknown request context remains inert data and cannot select programs or
commands. There are no consent operations.

SIGINT or SIGTERM stops admission, drains bounded work, reaps children, removes
the owned socket and releases the lease. Clean shutdown exits 0; storage or
cleanup failure exits 1. After abrupt death, the OS releases the lease but a
stale socket can remain: confirm the former daemon is dead before manually
removing that socket. Never remove a live owner's socket or lock inode.

For T3 only, `--control-stdin` enables bounded JSON lines for shutdown and a
controlled clock. Set `clock` to
`{"source":"controlled_stdin","initial_unix_seconds":100}` only with that
flag and an exact 127.0.0.1 listener. `{"operation":"set_time","unix_seconds":160}`
advances time monotonically and emits `{"clock_unix_seconds":160}`.
`{"operation":"shutdown"}` or EOF shuts down; invalid control input fails
closed. No HTTP or callback clock control exists.

Rollback: stop cleanly, preserve store/tombstones/lock inode and operator
configuration, and revert this additive service change. No installed consumer
or active execution profile is migrated. Later gates own actual SDK/service
compatibility (T4-SDK), echo-bridge wiring (I-BRIDGE), and M7-T4/M7-T5. Review
owns independent validation, hosted terminal CI/coverage and merge.
