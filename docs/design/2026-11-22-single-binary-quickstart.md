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

1. **Download** the unlock's files and `boot-root.zip` from the thread's first post onto your PC.
2. **Unlock.** Run the unlock exactly as the first post says. **Stop when TWRP shows on the Echo's screen.**

   > **TODO screenshot (Echo screen):** TWRP's main menu, the point where you stop the unlock.

3. **Copy `boot-root.zip` to the Echo.** TWRP runs its own adb, so keep the USB cable in. Check that the PC sees it,
   then send the zip with TWRP's sideload:

   ```sh
   adb devices                 # one device, state "recovery"
   ```

   On the Echo, tap **Advanced > ADB Sideload** and swipe to start it. Then, on the PC:

   ```sh
   adb sideload boot-root.zip  # ends with "Total xfer: 1.00x"; TWRP shows the install log
   ```

   If sideload is not offered, push the file instead and install it from TWRP's **Install** screen:

   ```sh
   adb push boot-root.zip /sdcard/boot-root.zip
   ```

   Then tap **Install**, pick `boot-root.zip` from `/sdcard` and swipe to confirm.

   > **TODO screenshot (Echo screen):** TWRP's ADB Sideload screen, and the log after a successful install.

4. **Reboot.** Tap **Reboot > System**. Fire OS starts as before.
5. **Check root** from the PC:

   ```sh
   adb devices      # exactly one device, state "device"
   adb shell id -u  # prints 0
   ```

Nothing else from the thread or any other firmware project is needed.

## 2. Install

Keep the Echo connected over USB and run one line. The installer checks the release digest, pushes the one file
`echo-bridge`, sets up its start at boot, installs the screen app from inside that file and reboots the Echo.

```sh
curl -fsSLO https://github.com/portpowered/portos-echo/releases/latest/download/install.sh && sh install.sh
```

```powershell
irm https://github.com/portpowered/portos-echo/releases/latest/download/install.ps1 | iex
```

> **TODO screenshot (Echo screen):** the first screen after the reboot, with the sign-in code.

## 3. Sign in to ChatGPT

After the reboot, the Echo's screen and your terminal show a URL and a code. Open the URL on your phone or computer,
sign in to ChatGPT and enter the code. The installer continues on its own. If the code expires, run step 2 again; it
resumes where it stopped. To sign in again later, use **Settings > Accounts > OpenAI** on the Echo.

## 4. Talk to it

Say "Computer", then ask anything. It answers out loud, and you can interrupt it. The agent searches the web, reads
and edits files in its workspace, runs shell commands in a sandbox, uses its skills and your MCP servers, keeps plans
and memories, and looks through the camera when the shutter is open. To check everything at once:

```sh
echo-setup doctor   # ends with "tools: parity ok"
```

> **TODO screenshot (Echo screen):** the character listening after "Computer", and speaking a reply.

The agent has every Codex tool except code mode, the JavaScript `exec` tool. Code mode needs the V8 engine from
Chromium and Node, which we do not ship on the Echo's 32-bit ARM. The agent calls the same tools directly instead.

## 5. Connect Port OS (optional)

[Port OS](https://portoperatingsystem.com) lets the assistant control your smart home, and lets Port OS reach the
Echo as a device.

1. **Link your account.** On the Echo, open **Settings > Accounts > Port OS** and choose **Link Port OS**. The screen
   shows a URL and a code. Open the URL on your phone, sign in to Port OS, review the permissions it asks for and
   approve them.

   > **TODO screenshot (Echo screen):** Settings > Accounts > Port OS with the link code.

2. **Control your home by voice.** Say "Computer, turn off the kitchen lights." The agent calls the Port OS tools
   your link allows. If the Echo asks you to link again, a tool needs a permission the link lacks.
3. **Add the Echo to Port OS.** In the same screen, set **Name in Port OS**, tick **Connect this Echo to Port OS** and
   choose **Save Port OS endpoint**. The Echo then shows up in Port OS under that name.
4. **Use it from Port OS.** Start the assistant with text (the Echo speaks the reply), end a session, take a camera
   snapshot while the shutter is open, and set the volume or mute.
5. **Unlink** with **Remove**, then **Remove link**, in the same screen. This does not sign you out of ChatGPT.

## Addendum A: screen packs

A screen pack is a layout and an animated character for the Echo's screen. The default pack comes with the install.

**Install one from the Echo.** Open **Settings > Screen packs**. The Echo lists the packs published with the latest
release, with a preview of each. Search by name, tap a pack, then tap **Install**. The Echo downloads it, checks its
SHA-256 against the release index and switches to it. **Disable screen packs** goes back to the built-in screen.
Under **Advanced** you can still add a pack from any URL with its SHA-256.

> **TODO screenshot (Echo screen):** the pack gallery with search, and a pack's Install button.

**Install your own from a PC.** Unpack or build the pack into a directory named `<name>@<version>` and push it:

```sh
echo-setup pack push my-pack@0.1.0   # validates first, then prints "screen-pack: pushed my-pack@0.1.0"
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
echo-setup pack push ./my-pack@0.1.0
```

The full format, the validation rules and the companion pack's generator are in
[screen packs](../screen-packs.md).

## Addendum B: Wi-Fi in a new place

The Echo remembers the networks it has joined. In a new place:

1. Plug the Echo in. If it finds no saved network for 30 seconds, the screen opens **Wi-Fi** by itself. Otherwise
   open **Settings > Wi-Fi**.
2. Tap your network, enter its password and tap **Join**. The status line shows **Connected** and the agent comes
   back on its own; you don't need to sign in again.
3. To remove a network you no longer use, tap it and choose **Forget**.

> **TODO screenshot (Echo screen):** Settings > Wi-Fi with the network list and the password field.

No screen, or a hidden network? Connect the Echo over USB and run:

```sh
echo-setup wifi join "Network name"   # asks for the password; it is never shown or stored on the PC
echo-setup wifi status                # prints the network name and "connected"
```

## Addendum C: voice and prompt

All of these apply from the next conversation; nothing needs a restart.

- **Voice.** **Settings > Conversation > Live voice** lists every voice the bundled Codex release offers. Pick one
  and choose **Save settings**. `default` uses the service's own default voice.
- **How it talks.** **Settings > Voice instructions** is the prompt for the voice conversation: its personality,
  how long its answers are, which language it speaks. Edit it and choose **Save voice.md**.
- **How it works on tasks.** **Settings > Runner instructions** is the prompt for the agent that runs tools for it.
  Edit it and choose **Save runner.md**.
- **By voice.** Ask it, for example: "From now on, keep your answers short." The agent can edit its own voice
  instructions (the `prompts` tool group, on by default).
- **From a PC.** The same prompts are the files `home/prompts/voice.md` and `home/prompts/runner.md` under
  `/data/local/echo` on the Echo. A prompt that can't be read keeps the last good one.

> **TODO screenshot (Echo screen):** Settings > Conversation with the voice list, and the Voice instructions editor.

The other settings (listening time, the daily request cap, the delegated model) are in
[configuration](../configuration.md).
