# Port OS Echo

Port OS Echo replaces the assistant on a rooted Amazon Echo Show 8 (2nd gen, codename `athena`) with ChatGPT's
live voice model. You say "Alexa", the Show listens, a GPT-Live session answers in real time, and a delegated agent
reaches your devices through the Port OS MCP server and other MCP tools.

Status: planning. No product code yet. The first work is Phase F, the repository harness: lint, shape and
layering checks and CI, all of which must exist before any crate does (`docs/program/plan.md`, section 6).

## How it fits together

```text
Echo Show (Rust + C shim)                   Gateway (small trusted service)      OpenAI
  wake word (Pryon "Alexa")                   relays the SDP offer               GPT-Live session
  audio capture/playback, screen, keys  --->  creates the session with the  ---> delegated agent model calls
  WebRTC peer to GPT-Live                     operator's credential
  MCP client: executes every tool call  ---------------------------------------> Port OS MCP, other MCP servers
```

The Echo executes every MCP call itself. The gateway only handles the models. Section 3 of the plan explains why
a gateway is needed in every topology.

## Layout

| Path | What it holds |
|---|---|
| `docs/program/plan.md` | The program plan: architecture, decisions, milestones, test tiers, bench, CI gates, Phase F |
| `docs/program/coordinator.md` | How work runs: one coordinator agent builds the repository, using bounded `you` dispatches |
| `docs/program/rules.md` | Standing rules every change follows |
| `docs/program/baselines.md` | Measured facts about the device, the bench and the services |
| `docs/program/decisions.md` | Decision log with dates |
| `docs/design/echo-live-rfc.md` | The original RFC (draft; `plan.md` wins where they differ) |
| `docs/design/echo-live-execution-plan.md` | The original multi-agent execution plan (draft; `plan.md` wins) |
| `AGENTS.md` | Instructions for the coordinator and any agent working here |

The crate layout, `Makefile` and CI arrive with Phase F.

## Build, test, lint, deploy

Not available yet. Phase F lane `el-f1-readme-makefile` adds `make lint`, `make test-unit`, `make test-contract`,
`make test-sim`, `make build-host` and `make build-armv7`, and updates this section. Deploying to a device is
operator-run only (`docs/program/rules.md`).

## Related projects

- [echo-dot-assist](https://github.com/Gamer92000/echo-dot-assist) (MIT): the rooted Echo work this project builds
  on: board profiles, the `hassmic` C code, unlock and deploy scripts, and the Pryon wake word path.
- [Port OS](https://portoperatingsystem.com): the smart-home MCP server and the `port` CLI that drives the test
  bench ([portos-cli](https://github.com/portpowered/portos-cli)).

## License

Apache License 2.0. See [LICENSE](LICENSE).
