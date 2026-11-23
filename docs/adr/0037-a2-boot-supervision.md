---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 23
status: accepted for the A2 boot role
---

# ADR 0037 A2 boot supervision

`echo-bridge boot run` is a selectable Android root role. It keeps the existing
device and core entry points and uses the [A2 installation layout](../design/2026-11-22-single-binary-plan.md#6-installer).
The I/O-free `echo-boot` crate owns slot selection, the stop handshake and
restart timing. The bridge supplies the clock, filesystem and children through
`HostOps`. Journal encoding uses the workspace's existing serde dependencies.
The launcher uses rustix's locked process, filesystem and thread APIs in a
single-threaded re-exec before dropping credentials.

The supervisor records a current-slot attempt before launching core and
reconciles an unfinished attempt at startup. The journal uses a SHA-256 digest
of `bin/current` as its generation. Core must emit the existing
`{"event":"bridge.ready"}` line on its own stdout and remain alive for 60 s to
clear failed tries. The third failed try prevents further current starts.
After reaping the old pair, the supervisor validates `bin/previous`, records
its selection and logs the two digests. Promotion remains an installer action.
A root-only journal is atomically replaced and synced in `bin/.boot`.

Device runs the A1 satellite with `device -P wyoming -D -E` and the existing
Android Unix socket. It uses uid 1000, real gid 3990, effective and saved gid
1000, supplementary groups 1000, 1005, 3003, 1004, 1002 and
`u:r:hassmic:s0`. Core runs in its own session as uid 9999, gid 3990 and
supplementary groups 3990, 3003, 1004, using `private/bridge.toml`,
`private/bridge.trace` and `home/`. The device launcher requests its exec
context through the fixed Android shell entry point. The launcher pauses before
the selected role starts so the parent can verify session and credentials
and, for device, the SELinux domain. Failed setup stops the affected launch.

Stable children restart after 3 s. Successive quick exits double the delay up
to 300 s. Core keeps its stdin pipe until shutdown, when closing it lets the
existing shutdown path run. A stop marker at `private/run/stop` cancels the
attempt and parks both roles. The supervisor reaps the child groups before
writing `private/run/finished` as nobody. Removing the stop marker clears
the acknowledgement and resumes supervision. The inherited supervisor lock
prevents overlapping launches while an old child still holds it.

The timing, group choices and stop mechanism follow the fork's MIT-licensed
[satellite supervisor](https://github.com/portpowered/echo-dot-assist/blob/7e0c758/scripts/system/main.sh)
and [bridge supervisor](https://github.com/portpowered/echo-dot-assist/blob/c3b6c6c/scripts/device/bridge.sh).

## Deferred

- Firewall mode and the device startup services land in the subsequent
  stories of this lane. Screen APK embedding belongs to C1.
- The installer owns the stable init entry, SELinux policy, root-owned layout,
  slot promotion while parked and legacy restore. A2 T5 verifies that Fire OS
  permits the fixed shell entry and enforces the resulting domain, socket access,
  process cleanup and rollback with the installed init services.
