---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted; read-only calibration
---

# Five-target touch calibration

`echo-lab calibrate --adb <path> [--serial <s>] --out <fresh-dir> [--timeout-secs 120]`
records five gestures and fits raw coordinates to the 1280 by 800 panel. The
parent directory must exist. The command reserves its output directory before
starting adb and refuses to overwrite an existing run.

The command reuses probe's explicit adb argv builder and touch inventory parser.
It shares the driver's firmware predicate: only a probed build with normal boot
mode and completed Android boot is admitted. Every selected adb invocation uses
the same optional serial. Only `shell getprop` and `shell getevent` are used;
nothing is pushed, written or injected on the device. No dependency is added.

The operator taps the printed top-left (64,64), top-right (1216,64), bottom-right
(1216,736), bottom-left (64,736) and centre (640,400) positions in order. Each
prompt is flushed before capture continues. The command does not draw targets.
Complete BTN_TOUCH or tracking-ID gestures use the last fresh X/Y pair before
release. Five taps on one node end capture. Multiple contributing devices are
ambiguous. Timeout and incomplete input retain partial evidence without a fit.

Artifacts are local `getevent.log` and `calibrate.json`. The JSON records firmware,
device nodes/names/ranges, raw taps and targets, glass identity and per-axis
least-squares scale, offset and maximum residual. The fit maps raw to panel
coordinates. A maximum residual of exactly 24 pixels passes; greater residuals
fail while retaining the fit. Failed or incomplete sessions exit nonzero.
The host monotonic deadline bounds property reads and capture. Every terminal
path terminates and reaps a started child and joins its readers. Report
publication uses a temporary file and rename in the reserved directory.

Keep artifacts outside git: raw touch data and device metadata are operator
records. There is no automatic retry. Revert this PR to remove the command;
existing probe manifests and driver behavior are preserved.

Workers validate with the prebuilt CLI and synthetic adb only (T3), plus pure
reducer/fit and injected-clock tests. These establish software behavior and
read-only command selection. Actual glass identity, human placement, Android
variants and Ring video require a separately operator-started manual session
once per firmware after merge. Independent loopback, terminal CI and merge are
owned by review; physical proof is owned by the operator.
