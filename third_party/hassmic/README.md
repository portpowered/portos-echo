# hassmic Wyoming satellite

Source repository `https://github.com/portpowered/echo-dot-assist`, commit
`c3b6c6cc7103aaf56492862d29355f5c311d6c2b` from the repository's `hassmic.pin`.
`sources.sha256` records the complete file inventory with each upstream
blob's SHA-256 and source path, followed by the local path when it differs.
`version` records that commit's UTC version for Wyoming Info.

Refresh from a clean local checkout of the pin with the following command.
Without `HASSMIC_SRC`, the script uses `.cache/hassmic-src/<commit>` at the
repository root. It verifies the entire inventory before publishing files.

```sh
HASSMIC_SRC=/path/to/echo-dot-assist third_party/hassmic/sync.sh c3b6c6cc7103aaf56492862d29355f5c311d6c2b
```

Review the paths, notices and hashes before changing the pin. Committed
copies reproduce the upstream bytes except `tools/mkstubs.sh`. Running
`sync.sh` regenerates its non-executable copy at `tools/mkstubs.sh.txt`
from the pin using `shfmt -i 2 -ci`. The reference needs unpacked upstream
firmware and never runs in the build or CI. The build consumes the pinned
`.syms` declarations directly.

| Inventory paths | Licence and role |
| --- | --- |
| `LICENSE`, `src/hassmic/*`, `src/include/*`, `devices/athena/board.c`, `devices/athena/stubs/*.syms`, `tools/mkstubs.sh.txt` | Upstream MIT. The headers also include declarations required by common satellite code whose optional implementations are absent. |
| `src/third_party/monocypher.c`, `src/third_party/monocypher.h` | BSD-2-Clause or CC0-1.0 as declared in both files. The shared dispatcher links the arbitration helpers, which reference Monocypher even though Wyoming has no arbitration callback. |
| `src/third_party/minimp3.h` | CC0-1.0. `sounds.c` uses it to decode the satellite's sounds. A local translation unit provides the implementation that the fork builds in its ESPHome source. |

`micdenoise.c` uses the following RNNoise files. `rnnoise/COPYING` carries
the permissive BSD terms for the directory, and source files retain their
file-local notices where present.

| Files under `src/third_party/rnnoise/` | Role |
| --- | --- |
| `denoise.c`, `rnnoise.h` | Denoising implementation and public declarations. |
| `rnn.c`, `rnn.h`, `rnn_data.c`, `rnn_data.h`, `tansig_table.h` | Inference, model data and activation table. |
| `pitch.c`, `pitch.h`, `celt_lpc.c`, `celt_lpc.h` | Pitch and linear prediction. |
| `kiss_fft.c`, `kiss_fft.h`, `_kiss_fft_guts.h` | FFT implementation and declarations. |
| `arch.h`, `common.h`, `opus_types.h` | Internal types and platform helpers. |

The common dispatcher, settings and athena band code retain their upstream
interfaces to disabled services. The local `hassmic-stubs` provide inactive
callbacks for ESPHome-adjacent services, Sendspin and Music Assistant, BLE
and Improv, A2DP, Drop In, mDNS reannouncement, settings HTTP, Wi-Fi motion and
updates. The retained headers for these interfaces are included by
`main.c`, `settings.c`, `screen.c` or `audio_android.c`. The retained
`ws.h` declaration lets the arbitration object use the local random helper.
The service implementations, web assets, kernel code, SBC and aptX codecs
are absent. The Wyoming module retains its `-S` XML output.

The build selects Wyoming and disables the excluded service defaults in an
`OUT_DIR` copy of the shared dispatcher. An explicit non-Wyoming `-P` or a
Sendspin pairing request fails before startup. The usage line and Wyoming
implementation are byte-identical to the pin. Linux uses the fork's
file-audio and no-Pryon backends. The athena build uses Android NDK API 24
OpenSL ES and generated Pryon and zlib linker inputs. The Opus declaration
file stays with the pin's inventory. Bridge media already links workspace
Opus, and the Wyoming satellite has no call to the fork's Opus consumers.
