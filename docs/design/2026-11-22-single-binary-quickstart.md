---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 22
status: proposed; the target README quick start once the single-binary plan ships
---

# Quick start (target)

This is the README quick start as a user will see it after
[the single-binary plan](2026-11-22-single-binary-plan.md) ships. Nothing here works yet.

## What you need

- An Echo Show 8, 2nd gen (2021, model A8H3N2, code name `athena`) on Fire OS 7.
- A Windows or Linux x86_64 PC with `adb` (Android platform-tools) and a USB cable.
- What the unlock thread lists for the unlock itself (step 1).
- A ChatGPT account with Codex and ChatGPT Live access, and Wi-Fi. A Port OS account is optional.

## 1. Root the Echo

The unlock is saiyaku-athena, a public exploit for the Echo's MT8183 chip by its XDA authors, not by this project.
Its author asks that it only ever be run by hand from the
[XDA thread](https://xdaforums.com/t/unlock-root-twrp-unbrick-amazon-echo-show-8-2nd-gen-2021-athena.4804958/),
never from an installer or another guide, so its commands are not repeated here.

1. Download the unlock's files and `boot-root.zip` from the thread's first post.
2. Run the unlock exactly as the first post says. **Stop when TWRP shows on the Echo's screen.**
3. In TWRP, choose **Install**, pick `boot-root.zip`, swipe to confirm, then choose **Reboot > System**.
4. When Fire OS has started, check root adb from the PC:

   ```sh
   adb devices      # exactly one device, state "device"
   adb shell id -u  # prints 0
   ```

Nothing else from the thread or any other firmware project is needed.

## 2. Install

Connect the Echo over USB and run one line. It checks the release digest, pushes the one file `echo-bridge`, sets
up its start at boot, installs the screen app from inside that file and reboots the Echo.

```sh
curl -fsSLO https://github.com/portpowered/portos-echo/releases/latest/download/install.sh && sh install.sh
```

```powershell
irm https://github.com/portpowered/portos-echo/releases/latest/download/install.ps1 | iex
```

## 3. Sign in

The Echo's screen and your terminal show a URL and a code. Open the URL on your phone, sign in to ChatGPT and enter
the code. If the code expires, run step 2 again; it resumes. To link Port OS too, enter the second code the screen
shows in Port OS.

## 4. Talk to it

Say "Computer", then ask anything. It answers out loud, and you can interrupt it. The agent searches the web, reads
and edits files in its workspace, runs shell commands in a sandbox, uses its skills and your MCP servers, keeps plans
and memories, and looks through the camera when the shutter is open. To check everything at once:

```sh
echo-setup doctor   # ends with "tools: parity ok"
```

The agent has every Codex tool except code mode, the JavaScript `exec` tool. Code mode needs the V8 engine from
Chromium and Node, which we do not ship on the Echo's 32-bit ARM. The agent calls the same tools directly instead.

## Addendum A: screen packs

A screen pack is a layout and an animated character for the Echo's screen. The default pack comes with the install.
The release publishes more as `<name>-<version>.screenpack.tar.gz` assets, with their SHA-256 in `SHA256SUMS`.

**Install one from the Echo (no PC).** Open **Settings > Load screen packs**. Under **Add from URL**, enter the
asset's URL (`https://github.com/portpowered/portos-echo/releases/download/<tag>/<asset>`) and its SHA-256, then
choose **Download pack**. Choose **Refresh packs** until it shows as installed, select it, choose **Use screen pack**
and **Back**. The Echo checks the digest before it unpacks anything. **Disable screen packs** goes back to the
built-in screen.

**Install one from a Linux PC.** Unpack the archive into a directory named `<name>@<version>` and push it, then
select it on the Echo as above:

```sh
mkdir orange-kitten@0.1.0
tar -xzf orange-kitten-0.1.0.screenpack.tar.gz -C orange-kitten@0.1.0
sh scripts/echo-screen-pack.sh push orange-kitten@0.1.0   # prints "screen-pack: pushed orange-kitten@0.1.0"
```

**Build your own.** A pack is a directory with a `pack.json` (`format: 1`, `name`, `version`, and a `layout`, a
`character`, or both), a still `rest` image (PNG or WebP), one WebM or MP4 clip per state (idle, listening, thinking,
speaking and so on) and, for lip sync, `closed`/`half`/`open` mouth stills. A layout places the clock, date and status
in percent of the screen and may add a stylesheet that uses a small allowed subset of CSS. Start from the generated
template, copy it under your own name, set `name` and `version` in its `pack.json` to match, replace the art, and
push it. The push validates the pack first and refuses anything outside the format:

```sh
sh scripts/make-template-screenpack.sh --version 0.1.0 --out ./generated   # needs Python 3 and ffmpeg
cp -r ./generated/template@0.1.0 ./my-pack@0.1.0                          # then edit pack.json and the art
sh scripts/echo-screen-pack.sh push ./my-pack@0.1.0
```

The full format, the validation rules and the companion pack's generator are in
[screen packs](../screen-packs.md).
