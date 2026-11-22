---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 22
status: accepted
---

# Vendored Wyoming satellite

The A1a device role runs the pinned hassmic Wyoming satellite as a separate
process of the bridge executable. Bridge dispatch hands raw OS arguments to
the role before it loads configuration, creates a runtime or starts services.
The device process keeps the satellite's signal handlers and exit status.

`echo-device` owns the role and `echo-device-sys` owns the C build and FFI.
The FFI entry takes mutable, NUL-terminated argument storage and keeps it live
through process exit. The file-audio host build uses the athena identity and
the existing Wyoming protocol. The device build uses the Android audio and
Pryon backends. Its generated link inputs reproduce the required stock
SONAMEs without firmware in the build or stand-ins in the release.

The C source and notices are pinned in the
[hassmic inventory](../../third_party/hassmic/README.md). Optional services
outside the satellite have local MIT shims. Existing bridge media code keeps
its own workspace Opus provider.

`echo-device` and `echo-device-sys` are local workspace packages. The C build
reuses `cc` 1.11.2 from the workspace lockfile.
