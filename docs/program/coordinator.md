---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: active; how work runs in this repository
---

# The coordinator and its runtime

One coordinator agent builds this repository. It plans each piece, writes the code itself or hands a bounded piece
to a `you` dispatch, checks the result, opens the pull request and merges it once CI is green. There are no
project leads, no integration lead and no per-lane owner agents. The original execution plan's roles (integration
lead, R1–R8 lane owners, `docs/design/echo-live-execution-plan.md` section 1) are replaced by this document. Its
lanes survive only as names for areas of the code.

## Why one coordinator

- **One owner for everything shared.** Contracts, `Cargo.lock`, the C ABI, CI and the crate list change in one
  place, by one agent, so nothing has to be arbitrated between leads.
- **The gates do the policing.** Shape, layering, rules and coverage checks run in CI (plan section 6.6). A rule
  that lives only in a prompt is not enforced, so the coordinator does not rely on reviewers remembering rules.
- **Parallelism is a tool, not a structure.** When pieces are independent, the coordinator fans them out for
  minutes, then integrates. No agent lives across tasks or owns a part of the tree.

## The runtime

The coordinator is a Claude Code session working in a git worktree of this repository. It uses the `you` CLI's
packaged one-shot factories for bounded work. It does not run a long-lived factory daemon, batches or lanes.

| Need | Command | Notes |
|---|---|---|
| Implement one bounded change in a worktree, iterating until it passes its check | `you run --named @you/goal "<brief>" --executor-provider codex --executor-model <model>` | Run from inside the worktree; codex refuses a directory that is not a git repository |
| Independent review of a diff or document | `you run --named @you/review "<brief>" --writer-provider codex --reviewer-provider claude` | The reviewer must not be the writer |
| Several independent pieces at once | `you --json run --named @you/spawn "<brief>" --count <n> --model-provider codex --worker-model <model>` | Give each piece its own paths; merge one at a time |
| Read-only research or a quick answer | `you run --named @you/subagent "<question>" --worker-provider codex --working-root <dir>` | Use `--no-record` when the prompt names credentials or private data |

Add `--no-record` whenever a brief or output may hold something sensitive. Model names come from
`you workers list`.

### A dispatch brief

Every dispatch gets a brief with these parts, because the worker sees nothing else:

1. **Goal**, in one sentence.
2. **Paths** it may change, and paths it must not touch.
3. **Acceptance**: the `echo-lab` scenario and test tier that must pass, or the exact command and its expected
   output. "Compiles" is never acceptance.
4. **Rules**: "follow `docs/program/rules.md`", which every worktree has because it is committed.
5. **Stop condition**: what to report instead of guessing (a contract change, a new dependency, a hardware
   question).

### The loop for one change

1. Pick the next item from the current phase in `docs/program/plan.md`.
2. Write the brief, or decide to build it directly when it touches shared contracts or is small.
3. Build or dispatch. For a dispatch, read the diff and run the acceptance yourself; never trust a report alone.
4. Push a branch, open a pull request that names the item, and let CI run.
5. Merge with `gh pr merge --squash` only when every required check is green. Never merge with `--admin`, and
   never weaken a gate or add a baseline to get a change through.
6. Record evidence: each milestone has `docs/program/milestones/<id>.md` with the scenario, the run's report
   directory and the result.

## Hardware and real accounts

Only the coordinator touches the Echo Show, the Ring camera or real accounts, and only for T5 runs (plan section
4.1) that the operator has started.

- Check the clock first. No audio on household devices before 09:00 local time.
- Runs are serialized: one Echo, one Ring, and Ring locks out after repeated sessions.
- Never change partitions, the bootloader or firmware. Never flash without the operator's explicit go-ahead.
- Workers from `you` dispatches never run against hardware or real accounts. They run tiers T0–T4 only.

## When the coordinator asks the owner

- A decision listed as open in `docs/program/decisions.md`.
- Anything that spends money, publishes, or acts on an account beyond the test principal.
- A plan change that drops a milestone or a gate.

Everything else, the coordinator decides and records in `docs/program/decisions.md`.

## Scaling up later

If one coordinator becomes the bottleneck, the next step is the full you-agent-factory (plan → process → review
lanes on a long-running daemon). That needs a `factory/` directory adapted for Rust, and a daemon port other than
7437 so that it can run beside the Port OS factory. Do not add it before the gates in plan section 6.6 exist.
