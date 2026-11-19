---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 19
status: in progress; configuration contract implemented, installer and renderer pending
---

# Screen packs

Screen packs are downloadable layout and character data for the bridge's screen page.
The bridge owns their installation and rendering. No pack code or character art belongs
in hassmic or the echo-screen APK.

## Download configuration

`[screen] packs` defaults to an empty list, preserving existing configurations.
Each entry has exactly two required string fields: `url` and `sha256`.
The URL must use HTTPS without inline credentials, whitespace, backslashes or a fragment.
The digest must contain exactly 64 hexadecimal digits; uppercase digits are accepted.
Nonempty downloads require an absolute `[home] dir`.

```toml
[screen]
pairing_token_file = "private/screen-pairing.txt"
packs = []
```

A release pack will use a URL ending in `<name>-<version>.screenpack.tar.gz`.
Its digest pins the archive bytes. A placeholder digest never bypasses verification.
Keep the list empty until the deployed bridge includes the installer. Parsing this
configuration performs no downloads and does not open credential files or create a home.

The installer must use only the configured `codex.ca_file` bundle, disable proxy use,
verify the digest before extracting anything, and reject unsafe archives. Installation,
catalog routes, previews, download status and reload events remain implementation work.
No runtime installation or media playback is claimed by the configuration support alone.

## Sparse selection settings

The optional root `screen_pack` key in `<home>/settings.toml` selects a pack:

```toml
screen_pack = "template@0.1.0"

[voice]
session_listen_enabled = true
```

- Omitted: the screen owner will choose an installed template automatically, otherwise
  the built-in page. Resolution against the installed catalog is pending.
- Empty string: explicitly disable packs.
- A bare name: select an exact local name, with `template` reserved for automatic
  template version resolution.
- `name@version`: select an exact installed version.

Names start with a lowercase ASCII letter, contain only lowercase letters, digits and
hyphens, and have at most 48 characters. Versions have three nonempty decimal components
and an optional prerelease made of dot-separated nonempty ASCII alphanumeric components.
Paths, separators, extra `@` characters and build suffixes are rejected.

The bridge's existing settings writer preserves this key across voice or cap changes.
Selection-only persistence does not request a voice restart. Existing voice and cap
changes retain their restart behavior. Invalid settings reject the entire overlay and
retain the validated base with a safe diagnostic. Before rolling back to a bridge with
the older strict settings parser, remove the root `screen_pack` key.
