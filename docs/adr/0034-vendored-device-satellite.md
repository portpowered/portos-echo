---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 22
status: accepted
---

# Vendored Wyoming satellite

The device role runs as a separate process of the bridge executable. Bridge
dispatch hands raw OS arguments to the role before it loads configuration,
creates a runtime or starts services. The device process keeps the satellite's
signal handlers and exit status.

`echo-device` owns the role and `echo-device-sys` owns the C build and FFI.
The FFI entry takes mutable, NUL-terminated argument storage and keeps it live
through process exit. A1a linked the pinned C Wyoming implementation. A1b
registers the Rust Wyoming protocol before native startup. The Rust provider
dispatches events and publishes state through the pinned framing helpers. The
C core owns connection accept and close, capture, wake, controls, playback and
connection generation. Protocol callbacks, command dispatch and writes use
the existing core lock. A failed write shuts down the borrowed connection
so its reader can unlink it before the C listener closes it.

The build excludes `proto_wyoming.c` while retaining it in the pinned
inventory. The supervisor arguments carry forward. The file-audio host
build uses the athena identity and keeps the TCP default on port 16700.
The device build uses the
Android audio and Pryon backends. Its generated link inputs reproduce the
required stock SONAMEs without firmware in the build or stand-ins in the
release.

On Android, `hassmic -P wyoming -D` listens on
`/data/misc/portos-echo-device/wyoming.sock`. The launcher gives the device
UID `system`, real GID `3990` and effective GID `system`. The device uses
GID `3990` while creating the private directory (`0750`) and socket (`0660`),
then restores its effective group before initializing native audio. The bridge
runs as `nobody` with group `3990`, so it can traverse the directory and connect.
The directory location assumes the Android `/data/misc` ownership and traversal
permissions. Coordinator gate `R-A1-T5` verifies those permissions and SELinux
access under the actual Fire OS launcher.

`--wyoming-socket PATH` selects a Unix listener at an absolute filesystem path
whose parent already exists, except that the Android default creates its own
directory. Unix mode accepts legacy `-p` and suppresses the TCP service from
`-S`. An owner-only lock file next to the socket serializes bind, stale recovery
and cleanup. A refused socket can be reclaimed after confirming its inode. On
clean exit the device removes only the socket it bound. The bridge retries
Unix on each connection and uses loopback TCP only on absence or refusal for
one release, so restoring the pinned fork binary still works.

The C source and notices are pinned in the
[hassmic inventory](../../third_party/hassmic/README.md). Optional services
outside the satellite have local MIT shims. Existing bridge media code keeps
its own workspace Opus provider.

`echo-device` and `echo-device-sys` are local workspace packages. The C build
reuses `cc` 1.11.2 from the workspace lockfile.
