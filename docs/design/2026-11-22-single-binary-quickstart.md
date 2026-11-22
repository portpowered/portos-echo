---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 22
status: proposed; the target README quick start once the single-binary plan ships
---

# Quick start (target)

This is the README quick start as a user will see it after
[the single-binary plan](2026-11-22-single-binary-plan.md) ships. Nothing here works yet.

You need an Echo Show 8 (2nd gen, 2021), a Windows or Linux x86_64 PC with `adb`, a ChatGPT account with Codex and
ChatGPT Live access, and Wi-Fi. A Port OS account is optional.

1. **Root the Echo.** Follow the `athena` device page of the echo-dot-assist fork up to and including root. Stop
   there: you do not install hassmic or edit `hassmic.conf`. Check that `adb devices` lists one device and that
   `adb shell id` reports `uid=0`.
2. **Run the installer.** Linux: `curl -fsSLO .../releases/latest/download/install.sh && sh install.sh`. Windows:
   `irm .../releases/latest/download/install.ps1 | iex`. It checks the release digest, pushes one file
   (`echo-bridge`), sets up the boot start, installs the screen app from inside that file and reboots the Echo.
3. **Sign in to ChatGPT.** The Echo's screen and your terminal show a URL and a code. Open the URL on your phone,
   sign in and enter the code. If it expires, run the installer again; it resumes.
4. **Optional: link Port OS and pick a character.** The screen shows a second code for Port OS. Under
   **Settings > Load screen packs**, add a pack by URL and digest.
5. **Say "Computer".** Ask anything. It answers out loud, and you can interrupt it. Its agent searches the web, reads
   and edits files in its workspace, runs shell commands in a sandbox, uses its skills and your MCP servers, keeps
   plans and memories, and looks through the camera when the shutter is open. `echo-setup doctor` ends with
   `tools: parity ok` when every Codex tool is available.

The agent has every Codex tool except code mode, the JavaScript `exec` tool. Code mode needs the V8 engine from
Chromium and Node, which we do not ship on the Echo's 32-bit ARM. The agent calls the same tools directly instead.

## What changed versus today

| Today ([README](../../README.md) steps 1–3) | After the plan |
| --- | --- |
| Step 1 roots the Echo and installs the fork's `hassmic` with its `scripts/setup.sh` and `install-system.sh` | Step 1 stops after root. `echo-bridge` captures, plays, wakes and supervises itself |
| You edit `/data/local/hassmic/hassmic.conf` (`PROTO=wyoming`, `ARGS="-D"`, `LISTEN_FROM`) and reboot | No hassmic configuration. No Wyoming port exists on the network |
| The Echo bundle holds `echo-bridge`, a separate `codex-app-server`, a CA bundle and `echo-screen.apk` | One file, `echo-bridge`. The Codex app server, the CA bundle and the APK are inside it |
| The fork's `bridge.sh` starts the bridge after boot; hassmic's service hosts the screen app | `echo-bridge` owns its boot start, restarts and updates. The screen app is the Echo's home app |
| The agent has the bridge's tools and web search; Codex's own tools are off | The agent has the full Codex tool surface except code mode |
| Sign-in, Port OS and character packs | Unchanged |
