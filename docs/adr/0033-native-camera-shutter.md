---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 21
status: accepted
---

# Native camera shutter boundary

## Context

The non-root bridge owns the physical shutter decision for snapshots. The
screen app owns Android Camera2 and sends one image over the paired screen
listener.

## Decision

The local `echo-camera` crate owns read-only Android evdev calls. It selects a
named switch device with the camera lens-cover capability. Linux host builds
use the bridge's unavailable path. On Android, an unreadable device, failed
ioctl or lost event history makes the shutter unknown. A new request gets a
fresh reading and its own event queue.

Linux 4.4 evdev can flush queued switch events on the descriptor used for a
state query. The boundary uses a separate descriptor to retain close and reopen
transitions. The ioctl on the borrowed live descriptor encodes the writable
array's exact length. Only successful results within that array are read.
Input events use the Android ABI's native timeval size and fixed-width event
fields. Readiness registration owns a duplicate descriptor that refers to the
same event queue throughout registration. The boundary uses the already locked
libc and Tokio versions.

The private screen exchange authenticates each operation with HMAC-SHA256 and
the existing pairing digest. Its scoped dependencies already appear in the
workspace lockfile.

## Packages covered

- `echo-camera` (new local workspace package)
