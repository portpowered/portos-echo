# Port OS Echo

Port OS Echo turns a rooted Amazon Echo Show 8 (2nd gen, `athena`) into a voice assistant.
Say "Computer": hassmic captures audio and plays replies; `echo-bridge` and its pinned Codex app-server child
connect to ChatGPT Live and execute permitted delegated tools through Port OS MCP.

The repository contains the bridge, screen client, configuration and simulated test harness.
Device milestones and provider compatibility remain subject to the independent gates in the
[program plan](docs/program/plan.md).

## Run it on your Echo

Use the [quickstart](docs/quickstart.md) for Linux prerequisites, paired release assets or source builds,
installation, ChatGPT sign-in, Port OS linking and lifecycle commands.
The [configuration guide](docs/configuration.md) covers file locations, every accepted key, access and permissions.
The bridge and Codex run on the Echo beside hassmic; the PC is the Linux build/install and bench host.
The screen is an Android WebView of the bridge's loopback page. Credentials stay in private on-device files.

## M3 simulated full-duplex voice

Build the local artifacts with `make hassmic-host` and
`cargo build --locked -p echo-lab -p echo-bridge --bins`, then run
`cargo run --locked -p echo-lab -- run tests/scenarios/m3-full-duplex.yaml --driver sim`
or `cargo run --locked -p echo-lab -- check --milestone M3`. Normal `make test-sim`
discovery includes the same scenario.

The report references scalar evidence in `events.jsonl`: native first playback,
one overlapping question in the same run, `hassmic-continue` before the cut stop,
3200 tail bytes captured after the old `played` and five tail frames received by
the controlled Live peer, the measured cut below 600 ms with a
200 ms send window, replacement playback, and a closed native mute window with
zero outbound frames. Input phrases map to deterministic synthetic PCM markers;
this T3 check proves local transport and playback, without speech recognition or
real-provider compatibility. Physical M3 remains an operator-run T5 gate.

## Layout

| Path | What it holds |
| --- | --- |
| `docs/program/plan.md` | The program plan: architecture, decisions, milestones, test tiers, bench, CI gates, Phase F |
| `docs/program/coordinator.md` | How work runs: one coordinator agent builds the repository, using bounded `you` dispatches |
| `docs/program/rules.md` | Standing rules every change follows |
| `docs/program/baselines.md` | Measured facts about the device, the bench and the services |
| `docs/program/decisions.md` | Decision log with dates |
| `docs/design/echo-live-rfc.md` | The original RFC (draft; `plan.md` wins where they differ) |
| `docs/design/echo-live-execution-plan.md` | The original multi-agent execution plan (draft; `plan.md` wins) |
| `AGENTS.md` | Instructions for the coordinator and any agent working here |

The Rust workspace and build/test targets are declared in `lanes.toml` and `Makefile`.

## Building

Install Rust through rustup. `rust-toolchain.toml` pins Rust 1.100.0 with the minimal profile, clippy and rustfmt.
Linux is the only supported platform for building, testing and running Echo.
Use native GNU Make, Python 3.12 (`python` and `python3` on PATH), Git and POSIX sh.
See [Linux hosts](docs/README.md#linux-hosts) for local operation.

The existing gates also require their provisioned tools: cargo-nextest, cargo-deny, shfmt, shellcheck, gitleaks,
Node/npm and the documentation checkers. The pinned
versions and setup commands are in `.github/actions/setup/action.yml`; the build does not install them.
`make coverage` is no longer a gate; run by hand, it also needs cargo-llvm-cov and the pinned nightly toolchain.

Hosted CI is the quality gate. Run focused targets for changed paths; do not run a full local `make ci`.
The required jobs are defined in [.github/workflows/ci.yml](.github/workflows/ci.yml).

| Target | What it runs |
| --- | --- |
| `fmt-check` | Formatting checks |
| `lint-docs` | Metadata, index, style, links, Mermaid and accepted config-key coverage |
| `test-unit` | Workspace tests through nextest with zero retries |
| `test-contract` | Contract test selector |
| `test-sim` | Simulated suites and host scenarios with local fake providers |
| `build-host` | Locked workspace host build |
| `build-echo` | Android ARMv7 bridge build with provisioned NDK |
| `build-codex-armv7` | Pinned standalone Codex app-server and CA bundle |
| `build-echo-screen` | Android screen APK with provisioned SDK |

Device operations are operator-run only ([standing rules](docs/program/rules.md)).

## Related projects

- [echo-dot-assist](https://github.com/Gamer92000/echo-dot-assist) (MIT): the rooted Echo work this project builds
  on: board profiles, the `hassmic` C code, unlock and deploy scripts, and the Pryon wake word path.
- [Port OS](https://portoperatingsystem.com): the smart-home MCP server and the `port` CLI that drives the test
  bench ([portos-cli](https://github.com/portpowered/portos-cli)).

## License

Apache License 2.0. See [LICENSE](LICENSE).
