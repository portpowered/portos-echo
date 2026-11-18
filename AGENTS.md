# AGENTS.md

Instructions for the coordinator agent and every factory worker in this repository.

## Read first, in this order

1. `docs/program/plan.md`: what we are building, the decisions, the milestones and the current phase.
2. `docs/program/rules.md`: the standing rules. They apply to every change.
3. `docs/program/coordinator.md`: how work runs: one coordinator operates the you-agent-factory in `factory/`.
4. `docs/program/baselines.md`: measured facts. Cite them; do not rediscover them.
5. `docs/program/decisions.md`: what the owner decided, and what is still open.

`docs/design/` holds the original drafts. Where they disagree with `docs/program/plan.md`, the plan wins.

## If you are a factory worker

- Change only the paths your payload names. Stop and report instead of editing anything else.
- Your acceptance is the scenario or command in your payload, with the values it must print. Run it and paste the
  output in your final report.
- Never run against the Echo, the Ring or a real account. Tiers T0–T4 only.
- Stop and report on: a contract change, a new dependency, a hardware question, or a rule you cannot meet.

## Supported platform

Linux is the only supported platform for building, testing and running Echo.
Coordinator device runs use Linux pe-ci; workers never run hardware.

## Current phase

Phase F, the repository harness (`docs/program/plan.md` section 10). Nothing else starts before milestone M0.
The Echo runs echo-dot-assist's `hassmic` in Wyoming mode with `-D`; everything of ours runs in `echo-bridge` on
the operator's PC (plan sections 1–3).

## Notes

- Codex workers must run from inside a git worktree.
- The factory server for this repository is `http://127.0.0.1:7447`, never 7437.
- This repository is public: no secrets, serial numbers, account ids or personal details.
