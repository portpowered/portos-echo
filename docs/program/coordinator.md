---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 18
status: active; how work runs in this repository
---

# The coordinator and the factory

Owner decision (2026-11-15): work runs on the you-agent-factory, adapted from its baseline into `factory/`. One
coordinator agent is the single operator and integrator. It writes the work items, submits them to the factory,
watches them, checks every result itself and owns everything shared. The factory's workers plan, implement in their
own git worktrees, open pull requests, review and merge them. The original execution plan's roles (integration
lead, R1–R8 lane owners, `docs/design/echo-live-execution-plan.md` section 1) are replaced by this document. Its
lanes survive only as names for areas of the code.

## Who does what

| Role | Who | Model |
| --- | --- | --- |
| Coordinator: plan, contracts, `Cargo.lock`, the crate list, CI, batches, hardware runs, evidence | One Claude Code session | — |
| Planner (`plan` workstation) | Codex | `gpt-6.1-sol`; `gpt-6-astra` allowed for planning only |
| Processor, reviewer, validator and every other agent workstation | Codex | `gpt-6.1-sol` |
| Workspace setup, CI wait, routing | Scripts in `factory/scripts/` | — |
| Takeover of a stuck lane: finishes that lane's branch, then hands it to review | A direct subagent of the coordinator | — |

The worker list is `factory/factory.json`. A change to a worker's provider or model is a change to that file, and
it needs an owner decision unless it stays on Codex `gpt-6.1-sol`.

A lane is stuck after about 8 process visits without progress, when the same red `test-sim` cause repeats, or when
lanes on the same hot path block each other. The coordinator parks it with a request in its mailbox, waits for its
running session to end, and sends one direct subagent to finish the branch on the lane's worktree. The response then
names the final head and the lane goes to review. The coordinator itself writes no product code (decisions.md,
2026-11-18).

## Why one coordinator over a factory

- **One owner for everything shared.** Contracts, `Cargo.lock`, the crate list and CI change in one place, by one
  agent. Factory items never change them as a side effect.
- **The gates do the policing.** Shape, layering and rules checks run in CI (plan section 11); coverage is
  suspended (`decisions.md`, 2026-11-18). A rule that lives only in a prompt is not enforced, so nobody relies on
  reviewers remembering rules.
- **The factory gives parallel lanes without lane owners.** Independent items run at once in their own worktrees;
  dependent items wait on `DEPENDS_ON` edges until the upstream pull request has merged.

## Running the factory

The factory server for this repository listens on `http://127.0.0.1:7447`. Port OS's own factory uses 7437; never
point a command for this repository at it. Run every command from the repository root, on `main`, with a clean
tree: `setup-workspace` branches each item's worktree (`.claude/worktrees/<name>`) from current `main`.

| Need | Command |
| --- | --- |
| Check the configuration | `you factory config validate factory/factory.json` |
| Start the daemon | `you run --continuously --with-server --listen 127.0.0.1:7447 --record ~/.you-agent-factory/recordings/<yyyy-mm>/<name>.json` |
| Stop it gracefully | `curl -X POST http://127.0.0.1:7447/shutdown` |
| Submit a batch | `you submit batch <file.json> --server http://127.0.0.1:7447`, after the same command with `--dry-run` |
| See all work | `you work list --server http://127.0.0.1:7447` |
| One item, its sessions | `you work show <workId>`; `you worker-sessions list --work-id <workId>` |
| Watch progress | `curl -N -s -H "Accept: text/event-stream" http://127.0.0.1:7447/factory-sessions/~default/events`, filtered on `"type":"WORK_STATE_CHANGE"`; read `payload.workId`, `payload.fromState` and `payload.toState`. The events carry no `stateType` field. Skip events older than the watch: the stream replays its history first |

Batch shape, payload templates and the loopback are in `factory/docs/batch-inputs.md`,
`factory/docs/idea-payload-template.md` and `factory/docs/standards/validation-loopback-template.md`.

### What to expect (smoke test, 2026-11-15)

The factory was proven on two sample items and a loopback before any program work: both pull requests merged
(#3, #9), the loopback re-ran their acceptance on `main` and accepted, and every agent session ran Codex
`gpt-6.1-sol`. What that run taught:

- **Until `el-f10-ci` adds checks, every item waits about 10 minutes in `task:awaiting-ci`** (the no-checks grace)
  before review. Until then the reviewer runs the acceptance command itself before it merges.
- **A worker loads its script when it is dispatched.** A fix to `factory/` reaches new dispatches only; restart the
  daemon after merging one. `you work move` is refused while an item has an active dispatch.
- **Lane worktrees keep untracked `prd.json`, `prd.md` and `progress.txt`,** and the daemon writes
  `.you-agent-factory/`, `tasks/` and `__pycache__/` where it runs. `.gitignore` covers them. Remove a merged lane's
  worktree with `git worktree remove`.
- **`factory.json` names a model for every worker,** so the default model in `~/.you-agent-factory/config.json`
  never applies.

### Batches

1. **Fresh work is always an `idea`.** A `task` is only for restoring an item whose worktree and `prd.json`
   already exist; a fresh `task` dies at once and fails everything that depends on it.
2. **One batch per phase**, with `DEPENDS_ON` edges (`requiredState: complete`) between its items. A dependent
   item waits until its upstream pull request has merged, then branches from the new `main`.
3. **Every batch ends with a `thoughts` loopback** that depends on every idea in the batch. Its payload asks for
   the behavior the phase promised, by command and expected values, and requires any gap to be filed as
   corrective work with its own loopback. A merged pull request is not evidence that anything works.
   The loopback runs focused commands: the scenario, the crate's tests, a seeded failure. It cites the hosted CI
   run for `main` instead of running a full local `make ci`. A Codex session doing that from a fresh clone died
   at about 18 minutes, twice, on the W1 loopback (2026-11-15).
   Lane payloads say the same: the quality gate is hosted CI's required checks on the final head, never a local
   or pe-ci `make ci` (owner, 2026-11-17). Lanes dispatch focused/repeated Linux tests themselves on hosted
   Ubuntu, never park for coordinator Linux proof. pe-ci stays coordinator-scheduled for T5 and rare
   hosted-unsupported work.
4. **Item name = branch = pull request title = worktree directory**, kebab-case with the plan item's prefix
   (`el-f2-rust-strict`).

### Hosted targeted witnesses

Required hosted CI on the pushed final head is the quality gate. Require focused changed-path signs of life
and clean architecture following `rules.md`. No story requires a soak, native-load or flake cohort. Lane
payloads target Linux only for building, testing and running Echo. Windows support is retired by the
2026-11-18 owner decision; only the owner changes required checks. Coordinator device runs use Linux
pe-ci with explicit Linux adb/port paths and existing forwarding/safety assumptions. Physical reachability
remains coordinator T5 proof. Milestone T5 and release T6 definitions remain.
pe-ci stays coordinator-scheduled for T5 and rare hosted-unsupported work.

Lanes dispatch directly (either selector is optional; at least one is required):

```sh
gh workflow run targeted.yml --ref <lane-branch> -f 'nextest_filter=test(t3_m3_)' -f repeat=2
gh workflow run targeted.yml --ref <lane-branch> -f scenarios=m4-spoof-tool
# Selectors combine; ignored defaults true, repeat defaults 1 (integer 1-10).
gh run list --workflow targeted.yml --branch <lane-branch> --event workflow_dispatch
gh run view <id> --json headSha,url,status,conclusion
gh run view <id> --log
gh run download <id> --pattern 'targeted-reports-*' --dir <local-report-directory>
```

Match branch, dispatch time and head SHA; link the run in a PR comment. Review inspects each
`TARGETED_RESULT repeat=<n> exit=<code>` and downloaded scenario `report.json`. Scenarios are space-separated
names from `tests/scenarios`, with optional `.yaml` suffix. Each repeat executes both requested selections
once with zero test retries. Any failed repeat fails the job even if a later repeat passes. Reports upload
on failure too. Deliberately failing witnesses belong on isolated scratch branches.

One intermittent hosted failure permits `gh run rerun <id> --failed` once; file a persistent flake as its own
lane. Required red still blocks merge; no timer, deadline or test weakening. The processor stops after final
head pushed, non-draft PR open, CI started and feedback addressed. Review owns terminal checks, independent
proof and merge; do not wait or recheck terminal CI after the processor finish line.

### A payload

Workers see nothing but the payload and the committed repository, so every payload carries:

1. **Mission**: the problem with its evidence, the solution as ordered steps with non-goals, and the acceptance.
2. **Acceptance**: the `echo-lab` scenario and tier that must pass, or the exact command and the values it must
   print. "Compiles" is never acceptance. The processor's finish line is a pushed head, an open pull request and
   CI started; merging belongs to review.
3. **Contention**: the paths the item owns and the paths it must not touch. A lane that creates a crate also
   owns that crate's one line in `lanes.toml` and the generated `Cargo.lock` change. A lane that adds an ADR also
   owns its one `docs/README.md` index line. Leaving these out pauses the lane at planning (el-w3-muse-core,
   2026-11-15).
4. **Rules**: `docs/program/rules.md` and `docs/program/baselines.md`, which every worktree has because they are
   committed.
5. **Stop condition**: report instead of guessing on a contract change, a new dependency, a hardware question or a
   rule it cannot meet.

### The coordinator's loop

1. Take the next phase from `docs/program/plan.md` (Phase F, section 10, before anything else).
2. Make any contract or shared-file change itself first, as its own pull request.
3. Write and submit the phase's batch with its loopback.
4. Watch the event stream. A failed attempt is often retried by the factory; act only on an item that stays
   failed with no live session.
5. For each merged item, read the diff and rerun its acceptance; never trust a report alone.
6. Record evidence: each milestone has `docs/program/milestones/<id>.md` with the scenario, the report directory and
   the result.

Merges follow `docs/program/rules.md`: `gh pr merge --squash` only when every required check is green, never
`--admin`, never a weakened gate or a baseline file to get a change through.

## Hardware and real accounts

Only the coordinator touches the Echo Show, the Ring camera or real accounts, and only for T5 runs (plan section
8.1) that the operator has started. Factory workers run tiers T0–T4 only, and no payload names the device, the Ring
or a real account.

- Check the clock first. No audio on household devices before 09:00 local time.
- Runs are serialized: one Echo, one Ring, and Ring locks out after repeated sessions.
- Installing or removing hassmic writes `/system` and needs the owner's go-ahead each time.
- Never change partitions, the bootloader or firmware without the owner's explicit go-ahead.

## Port OS

portos-backend owns plan section 9. The coordinator asks the portos-backend coordinator for anything this
repository needs from Port OS and never works around it here.

## When the coordinator asks the owner

- A decision listed as open in `docs/program/decisions.md`.
- Anything that spends money, publishes, or acts on an account beyond the test principal.
- A plan change that drops a milestone or a gate.
- A factory worker that would leave Codex `gpt-6.1-sol` (other than `gpt-6-astra` for planning).

Everything else, the coordinator decides and records in `docs/program/decisions.md`.
