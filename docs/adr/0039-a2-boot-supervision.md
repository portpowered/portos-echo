---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 23
status: accepted for the A2 boot role
---

# ADR 0039 A2 boot supervision

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

Start `echo-bridge boot firewall` as a separate init service before
`boot run`. A root-only lock in `bin/.boot` admits one firewall writer.
The writer checks the live filter every 5 s and repairs drift with one
`-w --noflush` restore per changed family. It puts `echo_out` first in
OUTPUT, preserves stock chains and restores the required Wi-Fi transport rules
for DHCP and outbound connections. The fixed IPv4 and IPv6 rules follow the
fork's MIT-licensed [lockdown policy](https://github.com/portpowered/echo-dot-assist/blob/7e0c758/scripts/device/lockdown.sh).
The writer restricts TCP/5555 to root-local traffic and configures adbd
for USB only. If ip6tables is absent, it verifies IPv6 is disabled on all
interfaces and by default. A failed installed tool or an unconfirmed apply
produces an unhealthy log and is retried on the watch cadence. The supervisor
checks the live protections before startup and resume. A resolver read failure
still applies the baseline lock with no resolver exceptions.

The supervisor starts the device role while Android startup services run in a
separate child group. Alexa takeover disables only enabled packages from the
[Athena package list](https://github.com/portpowered/echo-dot-assist/blob/7e0c758/devices/athena/device.conf)
and saves the original package states and Android settings in
`bin/.boot/alexa.json`. Each original is synced before its mutation. The
installer can stop the supervisor and invoke the trusted boot image as root
with `__boot-launch restore-alexa`. Restore returns explicit enables to
`enabled`, default enables to `default`, and absent settings to absence.
Verified restores remove their journal entries. Failed restores keep their
entries for retry. The Athena Wi-Fi diagnostics broadcast uses the fork's
disable and restore values. The package and settings data come from the MIT
fork whose notice ships with the existing `echo-bridge notices` output.

Startup compares the SHA-256 of `bin/echo-screen.apk` with the installed base
APK. It runs `pm install -r` for a changed or missing installation and launches
the fixed screen activity, including the prior installation if an update fails.
Command failures log the affected stage. The staged file belongs to the
root-owned install layout.

The Wi-Fi observer reads link and resolver state and leaves reconnection to
Android. When the link and resolvers are already usable at admission, the
core can start immediately. A new resolver set settles for 5 s, allowing
the firewall watcher to pick it up. The first core start waits at most 10 s
even when Wi-Fi is offline. The trusted running boot image runs the existing
clock sync as uid 0 with gid 3990 after network admission and then every 6 h
while connected. Link loss or a 6 s deadline cancels a pending sync. Only a
successful adjustment writes the UTC RTC. Android keeps its existing time
zone. A stop acknowledgement follows cleanup of the startup and clock jobs
as well as both product roles.

## Deferred

- The bounded first-start wait replaces the fork's indefinite wait for a
  resolver so the local screen can start offline. The hassmic Home Assistant
  clock and zone feed, other models' Wi-Fi helpers, standby Kiosk updater,
  hassmic OTA and mDNS remain in the legacy chain.
- Native init-service and Bluetooth HAL teardown, wake locks, adaptive
  brightness and overscan from the fork remain part of the installer and
  physical integration assessment. This lane applies Athena's reversible
  package, settings and Wi-Fi diagnostics controls.
- Screen APK embedding belongs to C1. Screen HOME and band behavior
  belong to `sb-a2-screen-home`, and Wi-Fi scan and join belong to
  `sb-a2-wifi-settings`.
- The installer owns the stable init entry, SELinux policy, root-owned layout,
  slot promotion while parked and legacy restore. A2 T5 verifies that Fire OS
  permits the fixed shell entry and enforces the resulting domain, socket access,
  process cleanup, firewall enforcement and rollback with the installed init services.
- The installer provisions the A1 device's existing wake model and state
  locations on a bare rooted Echo. The launch keeps the A1 asset lookup.
