---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 19
status: accepted; operator-authorized screen-pack archive dependencies
---

# Screen-pack archive decoding

The bridge installs downloadable data packs without adding code or art to hassmic.
The operator approved `tar` and `flate2` with its Rust backend for this lane,
including their resolved closure. Keep installation in `echo-screen-server`;
the offline configuration crate gains no runtime or network dependencies.

Use `tar` 0.4.46 with default features disabled and `flate2` 1.1.11 with default
features disabled and `rust_backend` enabled. The new resolved packages are:

| Package | Version | License |
| --- | --- | --- |
| tar | 0.4.46 | MIT OR Apache-2.0 |
| filetime | 0.2.30 | MIT OR Apache-2.0 |
| flate2 | 1.1.11 | MIT OR Apache-2.0 |
| crc32fast | 1.5.5 | MIT OR Apache-2.0 |
| miniz_oxide | 0.9.1 | MIT OR Zlib OR Apache-2.0 |
| adler2 | 2.0.1 | 0BSD OR MIT OR Apache-2.0 |
| simd-adler32 | 0.3.10 | MIT |

`cfg-if` and `libc` are already locked transitive dependencies. `reqwest`,
`sha2`, `tempfile`, `tokio`, `serde` and `serde_json` are existing workspace
dependencies reused by the pack owner. No unrelated package version moves.
No native zlib or new native build tool is introduced. Existing license and
advisory checks remain authoritative; no exception or baseline is added.

Digest verification precedes archive parsing and extracted writes. Read raw tar
entries to reject links, special files, GNU/PAX extensions and sparse metadata.
Bound compressed transfer, decompressed bytes including padding, expanded file
payload, file count and clip size. Validate normalized paths, role-specific file
extensions, the manifest and conservative CSS before staging any extracted file.
The blocking validation owner cooperatively cancels and is joined at shutdown;
publication rechecks the screen lifetime and revocation gate.

HTTPS uses only the configured PEM roots, without proxies, retries or an HTTP
downgrade. Failed jobs produce fixed diagnostics, retain installed identities
and leave the screen listener alive. No real account or hardware is involved.
The retained settings writer, authenticated catalog/media routes and reload
integration remain in the same behavior story; the archive owner introduces no
second settings writer.
