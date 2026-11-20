---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 19
status: active; Linux operator guide
---

# Run Echo on your Echo Show

Echo uses the rooted Echo Show 8 (2nd gen, `athena`) as a voice assistant. hassmic handles the microphone,
speaker and wake word; `echo-bridge` and its pinned Codex app-server child run beside it on the Echo.
The Linux PC installs and maintains them. It need not stay in the audio path.
Start with the [configuration guide](configuration.md), especially voice access and permissions.

## Before installing

You need a probed, rooted Echo with the project's [hassmic fork](https://github.com/portpowered/echo-dot-assist)
already installed, Wyoming mode with `-D`, and working root adb access. This guide does not unlock the device,
change firmware or install hassmic. Follow the fork's installation procedure separately with the owner's approval.
The configured wake word is **Computer**; the [baselines](program/baselines.md) record the supported device facts.
Keep Wyoming restricted to loopback for this placement.

Use Linux with POSIX sh, Git, GNU Make, Python 3.12 and Android platform-tools (`adb`) on PATH.
Connect exactly one ready adb device. Root is used to prepare files; the bridge runs as `nobody` (uid 9999)
with groups `3990,inet`, through `/data/local/hassmic/runas`. Do not run the bridge as root.
The shared `nobody` identity is not a separate runner sandbox.

Usable ChatGPT access is required before normal startup. Successful sign-in does not establish Live entitlement;
see [voice agent access](configuration.md#voice-agent-access-and-what-it-may-do).
Port OS linking is a separate authorization, performed after the bridge starts.

All device and account commands below are operator actions. Factory workers only inspect their defining source.

## Choose release assets or source builds

### Paired release installation

When a release is published, download its `install.sh` asset from
[GitHub Releases](https://github.com/portpowered/portos-echo/releases), alongside the matching version's assets.
Select an actual published version; these instructions do not imply that one is already available.
Inspect the downloaded script before running it. The release script downloads the versioned ARMv7 archive and
`SHA256SUMS`, verifies the archive, installs its paired bridge/Codex/CA files, starts the bridge, and sets up the
screen if absent. Retain the extracted bundle's `scripts/echo-install.sh` for lifecycle commands, or use that
same script from a matching source checkout. To extract a separately downloaded paired archive on Linux:

```sh
tar -xzf echo-VERSION-armv7.tar.gz
```

Replace `VERSION` with the published version. Run from the extracted bundle for subsequent `scripts/` commands.
Prepare `bridge.toml` as described below before running the downloaded asset:

```sh
sh install.sh --config ./bridge.toml
```

The release asset embeds its version. The repository's unprocessed `scripts/install.sh` additionally requires
`--version VERSION`. Downloads require curl or wget, tar, sha256sum and ordinary coreutils.
The wrapper attempts **start before sign-in**. On a fresh installation it can report
`echo-release: start failed; installed files retained`. This is a partial installation, not readiness.
Use the stopped-bridge sign-in below, then start. If this failure prevented screen setup, install the paired APK:

```sh
adb install -r apk/echo-screen.apk
adb shell "printf '%s\n' org.portos.echoscreen > /data/local/hassmic/state/screen_home &&
  chown system:system /data/local/hassmic/state/screen_home &&
  chmod 644 /data/local/hassmic/state/screen_home"
adb shell am start -n org.portos.echoscreen/.MainActivity
```

`screen setup failed; bridge already started` means the bridge start succeeded but APK/home setup did not.
The commands above repair that step without replacing private state. The wrapper does not replace an already
installed screen APK; use `adb install -r` with the paired APK when updating it.

### Build from source

Use a checkout compatible with the current configuration reference. Provision the tools pinned in
[CI setup](../.github/actions/setup/action.yml) before building. The bridge needs Rust 1.100.0,
the `armv7-linux-androideabi` target, Android NDK 27.3.13750724 (Linux API-24 clang), CMake and cached dependencies.
Codex also needs Git, curl, file, patch, Perl, Python, tar and sha256sum; its helper fetches the pinned Codex source.
Set `ANDROID_NDK_HOME` to the installed NDK. Set `CODEX_ARMV7_WORK_DIR` and `CODEX_ARMV7_OUTPUT_DIR`
to distinct new or empty absolute directories **outside the checkout**, with existing parents:

```sh
export CODEX_ARMV7_WORK_DIR=/tmp/echo-codex-work
export CODEX_ARMV7_OUTPUT_DIR=/tmp/echo-codex-output
make build-echo
make build-codex-armv7
make build-echo-screen
```

The screen build needs Java and `ANDROID_HOME` pointing to Android SDK platform 34/build-tools 35.0.0.
Use an operator keystore via `ECHO_SCREEN_KEYSTORE` for a stable update identity; preserve it privately.
The Codex output includes `codex-app-server`, a PEM `cacert.pem` and redistribution notices.
The bridge output is `target/armv7-linux-androideabi/release/echo-bridge` (or under `CARGO_TARGET_DIR`).
The APK is `target/echo-screen/echo-screen.apk`.

## Prepare and install configuration

Copy [the Echo example](../examples/bridge.echo.toml) to a private local `bridge.toml` and edit it.
The example's Codex binary and CA paths predate the installer's `bin/` layout: change those two values to:

```toml
[codex]
home = "/data/local/echo-bridge/private/codex-home"
binary = "/data/local/echo-bridge/bin/codex-app-server"
launch = "standalone"
ca_file = "/data/local/echo-bridge/bin/cacert.pem"
```

Edit the existing section rather than appending a duplicate. Keep `[home]` separate at
`/data/local/echo-bridge/home`; credentials belong under `private/`.
Use loopback for `echo.host`, `screen.listen` and `mcp.operator_listen`, with their example ports.
Configure your deployment's MCP resource URL and only the needed scopes/tools. The example uses public pilot
endpoints and the development CLI OAuth client; it is not proof of account access or a production registration.
Review the [full reference](configuration.md#configuration-reference) before granting effects.

The Port OS device-code default requests `endpoint:read`, `endpoint:write`, `message:send`, `group:read`,
`group:manage`, `subscription:read` and `subscription:write`, and permits the eleven Port OS MCP tools:
`control`, `endpoint_query`, `endpoint_show`, `endpoint_manage`, `group_query`, `group_manage`, `guide`,
`subscription_list`, `subscription_inspect`, `subscription_cancel` and `subscription_label_edit`.
You can set narrower `mcp.scopes` and `mcp.server.allow_tools` lists; an explicit empty tool list permits no tools.
An existing grant keeps its granted permissions until you explicitly link again. These defaults request
subscription permissions but do not start an event subscription consumer. Real-light control is authorized for
installed use; factory validation uses local synthetic devices and never acts on real lamps.

For source builds, from the checkout:

```sh
sh scripts/echo-install.sh install --config ./bridge.toml \
  --bridge target/armv7-linux-androideabi/release/echo-bridge \
  --codex "$CODEX_ARMV7_OUTPUT_DIR/codex-app-server" \
  --ca "$CODEX_ARMV7_OUTPUT_DIR/cacert.pem"
sh scripts/echo-screen-install.sh
```

Expected install success: `echo-install: installed`; screen success: `echo-screen installed`.
The installer copies files and seeds missing prompt files; it does not validate or rewrite TOML.
It retains an existing `bridge.toml` unless given `--force`, and always retains existing prompts.
It creates private/home directories with mode 0700 and configuration/prompts with mode 0600.
There is **no bridge boot integration** in this installer. After a device reboot, start it explicitly.

## Sign in, start, link and speak

Stop before ChatGPT sign-in: the bridge and operator command cannot own the Codex home simultaneously.
Run the sign-in as the same non-root identity, with an interactive adb session:

```sh
sh scripts/echo-install.sh stop
adb shell -t /data/local/hassmic/runas nobody 3990,inet /data/local/echo-bridge/bin/echo-bridge signin --config /data/local/echo-bridge/private/bridge.toml
```

The terminal and screen show a URL and device code. Open that URL on a phone or computer, sign into the intended
ChatGPT account and authorize the code. Wait for `signed in` before leaving the command. The command has a
five-minute deadline. On denial, expiry or failure, repeat sign-in for a fresh code; do not reuse an expired code.
The app server keeps the credential in its private home. Importing a desktop credential with `import-codex`
is deliberately unsupported in this backend. `signin --browser` exists for browser-capable hosts;
the device-code flow is appropriate on the Echo.

```sh
sh scripts/echo-install.sh start
sh scripts/echo-install.sh status
adb shell /data/local/hassmic/runas nobody 3990,inet /data/local/echo-bridge/bin/echo-bridge link --config /data/local/echo-bridge/private/bridge.toml
```

Start requires the exact `echo-bridge` process name and a fresh hassmic `client connected` line within 20 seconds.
Status shows process identity, uptime, the sanitized hassmic link and five bridge log lines.
It is an observation command: exit 0 alone does not mean the bridge is running.
`link` talks to the running bridge's loopback control endpoint. The Echo shows the Port OS verification URL/code;
approve it on a phone under the intended Port OS account and review the requested scope consent there.
The CLI prints `Verification URL: ...` and `User code: ...` immediately, matching the fresh code on the screen.
Open that URL on your phone and enter the code. The CLI waits for approval and reports `MCP linked` only
after the grant is stored. Denied or expired approval reports an error without printing success; rerun the
same command to start a fresh link.
Wait for the screen's linked state. A denied or expired code needs a new link attempt.

Say **“Computer, what time is it?”** and check that the screen and spoken reply agree.
Conversation is the default; requests needing tools are delegated to the runner. Further speech during the
default 15-second listening window continues the same run. The separate Live idle timeout reuses the call
between wakes. Device/audio/provider success still needs the operator's actual observation.

## Change settings, update or remove

Open Settings on the screen for voice, continued-listening enable/duration, daily cap and tool-call cap.
Changes are saved in `home/settings.toml`. During a call they remain pending until a safe between-call boundary;
the screen reports applied or pending. If automatic replacement fails, stop/start applies the saved settings.
For other base settings, edit the local file, stop, reinstall with the same install command plus `--force`, then
start. A saved overlay still wins for its five keys; changing only the base does not override it.

For updates, stop, obtain compatible paired release artifacts or rebuild, reinstall and start.
Keep a private backup of configuration/state before updating. Plain install updates binaries/CA but retains
configuration and prompts. `--force` replaces only base configuration. To roll back, install the previous
compatible artifact set with the retained state and start; do not mix Codex and bridge release versions.

To unlink Port OS while the bridge is running:

```sh
adb shell /data/local/hassmic/runas nobody 3990,inet /data/local/echo-bridge/bin/echo-bridge unlink --config /data/local/echo-bridge/private/bridge.toml
```

Unlink is handled by the grant owner and revokes/removes its grant. It does not sign out of ChatGPT.
For removal:

```sh
sh scripts/echo-install.sh stop
sh scripts/echo-install.sh uninstall
```

Uninstall verifies stop and removes installed bridge artifacts while retaining `private/` and `home/`.
`sh scripts/echo-install.sh uninstall --purge` also deletes those directories, including credentials, audit,
notes, settings and prompts. Purge is destructive; local credential deletion is not provider-side revocation.
For a source-installed screen, `sh scripts/echo-screen-install.sh --uninstall` removes the home selection and APK.
Neither bridge uninstall nor screen uninstall removes hassmic.

## Troubleshooting

| Observation | Meaning and recovery |
| --- | --- |
| `no Home Assistant link` in hassmic `boot.log` | hassmic has no Wyoming client. This is also the signature when Echo's bridge is absent, not a requirement to install Home Assistant. Check bridge status, configuration and loopback Wyoming port. |
| Process not found by name | Lifecycle uses `ps -A -o PID,NAME` and exact `echo-bridge`, then checks uid. A renamed process is not a safe lifecycle match. Use installed binary paths and the supplied launcher; do not bypass a failed identity check. |
| SIGTERM around 17 seconds after an adb-launched start | The recorded attached-shell launch died with its adb session. Use the installer start path: `setsid` detaches and the launcher holds stdin open. Redirecting stdin to EOF also stops the bridge. |
| Network deadline despite working root networking | The non-root launch needs Android network group 3990 as well as inet. Use `runas nobody 3990,inet`; do not switch to root to hide the error. |
| TLS/certificate failure | The child has a cleared environment and cannot discover the Echo's system CA bundle. Set absolute `codex.ca_file` to the installed PEM bundle; it also supplies bridge OAuth/MCP trust roots. |
| Fresh release `start failed; installed files retained` | Complete the stopped-bridge sign-in and screen setup above, then start. Files may be installed even though the wrapper did not print success. |
| Home already owned / second start refused | Stop the existing owner and wait for cleanup before sign-in/start. Do not delete the lease or blindly kill an unknown process. |
| Invalid base / rejected settings | An invalid base prevents startup. An invalid overlay is rejected wholly and the validated base is retained with a warning. Correct the reported field and retry. |

These signatures and launch constraints come from [measured baselines](program/baselines.md) and
the [launch contract](architecture.md). Logs can contain private operational data;
keep them private and redact before sharing. A successful lint or status command does not prove microphone,
credential isolation, release availability or Live entitlement.
