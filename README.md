# Port OS Echo: ChatGPT on Amazon Echo Show

Port OS Echo puts ChatGPT on an Amazon Echo Show 8 (2nd gen, 2021, codename `athena`) in place of Alexa.
Read [why and how it works](https://portoperatingsystem.com/blog/chatgpt-on-alexa/) or follow the
[step-by-step guide](https://portoperatingsystem.com/blog/how-to-put-chatgpt-on-an-echo-show/).

Say **"Computer"** and talk: ChatGPT Live answers out loud, a delegated Codex agent handles requests that need tools
or live data, Port OS controls your home, and an animated character shows what the assistant is doing.

Everything runs on the Echo. The community firmware's `hassmic` handles the wake word, microphone and speaker.
`echo-bridge` and OpenAI's Codex app server run beside it and carry the conversation and the tool calls. The screen
app shows the bridge's page. Your PC only installs and maintains these components.

> [!CAUTION]
> **Early software.** v0.1.0 has been tested on one Echo Show 8. Rooting wipes the Echo, unlocks its bootloader and
> can brick it. Read what you run; there is no warranty. See [Known limits](#known-limits).

**See it working:** [demo video on YouTube](https://youtube.com/shorts/Dgh7j3M_-kc).

## Screenshots

Captured from the Echo's own screen. For the assistant in motion, watch the
[demo video](https://youtube.com/shorts/Dgh7j3M_-kc).

<!-- markdownlint-disable MD033 -->
<table>
  <tr>
    <td><img src="docs/img/screens/echo-kitten-idle.jpg" width="420"
      alt="Orange kitten character at rest on a yellow background, clock and date bottom right"><br>
      Orange kitten pack at rest</td>
    <td><img src="docs/img/screens/echo-kitten-listening.jpg" width="420"
      alt="Orange kitten character with a blue light band along the bottom edge"><br>
      Mid-conversation: the blue band shows it is listening or answering</td>
  </tr>
  <tr>
    <td><img src="docs/img/screens/echo-junco-idle.jpg" width="420"
      alt="Junco bird character on a branch, yellow background, clock and date bottom right"><br>
      Junco bird pack at rest</td>
    <td><img src="docs/img/screens/echo-companion-idle.jpg" width="420"
      alt="Cel-shaded companion character in a green jacket on a dark background"><br>
      Companion pack, included in the release</td>
  </tr>
  <tr>
    <td><img src="docs/img/screens/echo-default-idle.jpg" width="420"
      alt="Two large round glowing eyes on a dark background, clock and date bottom centre"><br>
      Default pack</td>
    <td></td>
  </tr>
</table>
<!-- markdownlint-enable MD033 -->

## What you need

- An Echo Show 8, 2nd generation (2021, model A8H3N2). Other Echo models are not supported.
- A Linux PC with direct USB access for the unlock (no VM or WSL), as the firmware fork requires. The fork can also
  finish from Windows once the Echo is unlocked.
- A Windows or Linux x86_64 PC with Android platform-tools (`adb`) for the installer. It can be the same PC.
- A ChatGPT account with access to Codex and to the ChatGPT Live preview (step 3).
- A [Port OS](https://portoperatingsystem.com) account for home control (step 5).
- Wi-Fi with internet access for the Echo.

## 1. Root your Echo and install hassmic

The Echo needs the community **echo-dot-assist** firmware, from
[our fork](https://github.com/portpowered/echo-dot-assist) of
[Gamer92000/echo-dot-assist](https://github.com/Gamer92000/echo-dot-assist). It replaces Alexa with `hassmic` and
firewalls the Echo off from Amazon. Use the fork's `main` branch: its boot service starts the bridge and gives it
the camera shutter access that snapshots need.

Follow the fork's own instructions; this repository does not restate the flashing steps:

1. Read the fork's [README](https://github.com/portpowered/echo-dot-assist#readme) (requirements, install, undo).
   Then read its [`athena` device page][athena], which covers the pinned Fire OS firmware, the by-hand unlock and
   root, and the install.
2. Run its guided setup, `scripts/setup.sh athena`, on the Linux PC. It stops for the unlock and root, which you do by
   hand as the linked XDA thread says, then installs `hassmic` with `scripts/install-system.sh`. To develop or build
   the fork, see its [DEVELOPMENT.md](https://github.com/portpowered/echo-dot-assist/blob/main/DEVELOPMENT.md).
3. You do not need Home Assistant. Skip the setup's final step of adding the Echo to Home Assistant.
4. Switch `hassmic` to Wyoming mode for a realtime client on the Echo itself. Set these lines in
   `/data/local/hassmic/hassmic.conf` over root adb, then reboot
   ([hassmic.conf reference](https://github.com/portpowered/echo-dot-assist/blob/main/docs/GUIDE.md#hassmicconf)):

   ```sh
   PROTO=wyoming
   ARGS="-D"
   LISTEN_FROM="127.0.0.1"
   ```

   `-D` keeps the microphone open while a reply plays, so you can interrupt. `LISTEN_FROM="127.0.0.1"` refuses
   Wyoming connections from the network: Wyoming is unauthenticated and only the bridge on the Echo needs it.

Before you continue, check that `adb devices` lists exactly one ready device and that `adb shell id` reports root.

[athena]: https://github.com/portpowered/echo-dot-assist/blob/main/devices/athena/README.md

## 2. Install the package

Each [GitHub release](https://github.com/portpowered/portos-echo/releases) publishes these assets:

| Asset | What it is |
| --- | --- |
| `echo-setup-<version>-linux-x86_64`, `echo-setup-<version>-windows-x86_64.exe` | The installer for your PC |
| `install.sh`, `install.ps1` | Bootstraps that download the matching installer, verify it, then run it |
| `echo-<version>-armv7.tar.gz` | The Echo bundle: bridge, Codex app server, CA bundle, screen app, example config |
| `default-<version>.screenpack.tar.gz`, `companion-0.2.2.screenpack.tar.gz` | Character packs (step 4) |
| `SHA256SUMS` | SHA-256 digests of all of the above |

Connect the Echo over adb, then run one of the following.

**Linux, one line.** `install.sh` checks the installer against `SHA256SUMS` before it runs it. Read it first:

```sh
curl -fsSLO https://github.com/portpowered/portos-echo/releases/latest/download/install.sh
sh install.sh
```

**Windows PowerShell, one line.** `install.ps1` does the same check:

```powershell
irm https://github.com/portpowered/portos-echo/releases/latest/download/install.ps1 | iex
```

**Manual download with your own checksum check.** Set `VERSION` to the latest release's version:

```sh
VERSION=0.1.0
base=https://github.com/portpowered/portos-echo/releases/latest/download
curl -fLO "$base/echo-setup-$VERSION-linux-x86_64"
curl -fLO "$base/SHA256SUMS"
sha256sum --check --ignore-missing SHA256SUMS
chmod u+x "echo-setup-$VERSION-linux-x86_64"
"./echo-setup-$VERSION-linux-x86_64" install
```

```powershell
$Version = "0.1.0"
$Base = "https://github.com/portpowered/portos-echo/releases/latest/download"
$Exe = "echo-setup-$Version-windows-x86_64.exe"
Invoke-WebRequest "$Base/$Exe" -OutFile $Exe
Invoke-WebRequest "$Base/SHA256SUMS" -OutFile SHA256SUMS
(Get-FileHash -Algorithm SHA256 ".\$Exe").Hash.ToLower()
Select-String -SimpleMatch $Exe SHA256SUMS
& ".\$Exe" install
```

The two digests printed by the last two PowerShell lines must match.

The installer downloads and verifies the Echo bundle and copies it to `/data/local/echo-bridge`. It installs the
screen app and starts the bridge as the unprivileged user `nobody`. It then shows two approval codes: ChatGPT
(step 3) and Port OS (step 5). Finally it asks you to say **"Computer"**. If a code expires or setup times out, run the
same `install` command again: it resumes where it stopped.

Without `--config`, the installer uses the release's example configuration, which uses the public Port OS endpoints and
installs the default character pack. To set your time zone or change other settings, prepare a `bridge.toml` and pass
`--config ./bridge.toml` (`$env:ECHO_SETUP_CONFIG` for the PowerShell one-liner). The
[quickstart](docs/quickstart.md#prepare-and-install-configuration) explains how, and the
[configuration guide](docs/configuration.md) lists every key.

Afterwards, `echo-setup status --json` shows what is installed and `doctor` runs read-only checks.
`update --version VERSION`, `rollback` and `uninstall` (add `--purge` to delete credentials too) maintain the
installation. Settings such as voice, listening window and daily cap are on the Echo screen under **Settings**.

## 3. Sign in to ChatGPT

The voice conversation and the delegated agent share one ChatGPT sign-in, held by the Codex app server on the Echo.
The installer starts it for you: the terminal and the Echo screen show a URL and a code. Open the URL on your phone
or computer, sign in to ChatGPT, enter the code and wait for the installer to continue. The code is valid for five
minutes; if it expires, run `install` again for a fresh one.

To sign in again later, choose **Link OpenAI** under **Settings > Accounts > OpenAI (GPT Live)** on the Echo screen,
or run `echo-bridge signin` on
the Echo as described in the [quickstart](docs/quickstart.md#sign-in-start-link-and-speak).

You need a ChatGPT account with access to Codex and to the ChatGPT Live preview. A successful sign-in does not prove
Live access. OpenAI may change who can use the preview and on what terms; see
[voice agent access](docs/configuration.md#voice-agent-access-and-what-it-may-do). Your credentials stay in private
files on the Echo.

## 4. Upload character packs

Character packs are downloadable layouts and animated characters for the screen. The default pack installs with the
release. These packs are published as GitHub release assets:

| Pack | Asset (in [v0.1.0](https://github.com/portpowered/portos-echo/releases/tag/v0.1.0)) | SHA-256 |
| --- | --- | --- |
| default 0.1.0 | `default-0.1.0.screenpack.tar.gz` | listed in the release's `SHA256SUMS` |
| companion 0.2.2 | `companion-0.2.2.screenpack.tar.gz` | listed in the release's `SHA256SUMS` |
| orange-kitten 0.1.0 | `orange-kitten-0.1.0.screenpack.tar.gz` | `6707445a222aec37f222018e9bd94819a596c893dd51bc9882c7af874384a7b2` |
| junco-bird 0.1.0 | `junco-bird-0.1.0.screenpack.tar.gz` | `2e750126cabacc7cc0c790d48694848d1cf94b21a30878f1db3f2bc072ff73f5` |

Each asset's URL is `https://github.com/portpowered/portos-echo/releases/download/v0.1.0/<asset>`.

**From the Echo screen (no PC needed).** Open **Settings > Load screen packs**. Under **Add from URL**, enter the
asset's URL and its SHA-256, then choose **Download pack**. Choose **Refresh packs** until the pack shows as installed.
Then select it, choose **Use screen pack** and **Back**. The Echo verifies the digest before it unpacks anything.
To go back, select another pack or **Disable screen packs**.

**From a Linux PC with adb.** Unpack the archive into a directory named `<name>@<version>` and push it with the
helper from a checkout of this repository. Then select it in **Settings > Load screen packs** as above:

```sh
mkdir orange-kitten@0.1.0
tar -xzf orange-kitten-0.1.0.screenpack.tar.gz -C orange-kitten@0.1.0
sh scripts/echo-screen-pack.sh push orange-kitten@0.1.0
```

The helper validates the pack before it touches the device and prints `screen-pack: pushed orange-kitten@0.1.0`.
To make your own pack, see [screen packs](docs/screen-packs.md): the format, the validation rules and the generators.

## 5. Connect Port OS

[Port OS](https://portoperatingsystem.com) lets the assistant control your smart home. It also lets Port OS reach
the Echo as a device.

1. **Link your account.** The installer shows a Port OS code after the ChatGPT sign-in. To link later, choose
   **Link Port OS** under **Settings > Accounts > Port OS**. Open the URL on your phone, sign in to Port OS, and
   review the requested permissions before you approve them. You can narrow them with `mcp.scopes` and
   `mcp.server.allow_tools` ([quickstart](docs/quickstart.md#prepare-and-install-configuration)).
2. **Control your home by voice.** Say "Computer, turn off the kitchen lights." The voice model hands the request to
   the delegated agent. The agent calls the Port OS MCP tools your grant allows, and the bridge executes every call on
   the Echo. If the Echo asks you to link again, a tool needs a permission the grant lacks; link again and review it.
3. **Add the Echo to Port OS.** In **Settings > Accounts > Port OS**, set **Name in Port OS**, tick **Connect this
   Echo to Port OS** and choose **Save Port OS endpoint**. The Echo registers as a named endpoint under the same
   link: it registers its own OAuth client, keeps a DPoP-bound refresh token and reads its route back from Port OS.
4. **Use it from Port OS.** You can start the assistant with text (the Echo speaks the reply) and end a session. You
   can take a camera snapshot while the shutter is open, and send volume and mute commands.
5. **Unlink** with **Remove**, then **Remove link**, under **Settings > Accounts > Port OS**. This revokes the grant.
   It does not sign you out of ChatGPT.

Details: [Port OS endpoint](docs/configuration.md#port-os-endpoint),
[camera snapshots](docs/configuration.md#camera-snapshots) and [troubleshooting](docs/quickstart.md#troubleshooting).

## Known limits

- **Firmware fork required.** Port OS Echo runs only on an Echo Show 8 (2nd gen) rooted with the echo-dot-assist fork
  in Wyoming mode with `-D` (step 1).
- **ChatGPT Live preview.** Voice depends on preview access through the Codex app server. Access and terms may change.
- **One voice session at a time.** A wake word and a Port OS remote start share one voice slot.
- **Remote volume.** Port OS volume and mute commands are acknowledged, but the current hassmic firmware does not
  apply or report the change, so the acknowledgement carries `NOT_SUPPORTED`.
- **Snapshots need the shutter open.** With the camera shutter closed, snapshots are refused (`SHUTTERED`).
- **Transcripts stay on the Echo.** Audio and text still pass through OpenAI to run the conversation.
- **Early software.** Tested on one Echo Show 8.

## Build from source

Building, testing and running from source is supported on Linux only; `echo-setup` also runs on Windows x86_64.
Install Rust through rustup (`rust-toolchain.toml` pins the toolchain), then follow
[Build from source](docs/quickstart.md#build-from-source). It covers `make build-echo`, `make build-codex-armv7`,
`make build-echo-screen` and `scripts/echo-install.sh`. `make dist DIST_VERSION=X.Y.Z` builds the release assets
listed in step 2. Hosted CI ([.github/workflows/ci.yml](.github/workflows/ci.yml)) is the quality gate; locally, run
focused targets such as `make test-unit`, `make test-sim` or `make lint-docs` for the paths you change.
Device operations are operator-run only.

## Documentation

| Document | What it covers |
| --- | --- |
| [Quickstart](docs/quickstart.md) | Install options, custom configuration, sign-in, lifecycle and troubleshooting |
| [Configuration](docs/configuration.md) | Files, every accepted key, voice agent access, Port OS endpoint, camera |
| [Screen packs](docs/screen-packs.md) | Pack format, validation, paired controls, local push, generators |
| [Architecture](docs/architecture.md) | Components, the launch contract on the Echo and the conversation lifecycle |
| [Program plan](docs/program/plan.md) | Architecture decisions, milestones and test tiers |
| [AGENTS.md](AGENTS.md) | Instructions for agents and contributors working in this repository |

## Related projects

- [echo-dot-assist](https://github.com/Gamer92000/echo-dot-assist) (MIT) and
  [our fork](https://github.com/portpowered/echo-dot-assist): the rooted Echo work this project builds on, including
  board profiles, the `hassmic` C code, the unlock and deploy scripts, and the Pryon wake word path.
- [Port OS](https://portoperatingsystem.com): the smart-home MCP server, and the `port` CLI that drives the test
  bench ([portos-cli](https://github.com/portpowered/portos-cli)).

## License

Apache License 2.0. See [LICENSE](LICENSE).
