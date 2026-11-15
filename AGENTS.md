# AGENTS.md

Instructions for the coordinator agent and any agent a `you` dispatch starts in this repository.

## Read first, in this order

1. `docs/program/plan.md`: what we are building, the decisions, the milestones and the current phase.
2. `docs/program/rules.md`: the standing rules. They apply to every change.
3. `docs/program/coordinator.md`: how work runs: one coordinator builds, using bounded `you` dispatches.
4. `docs/program/baselines.md`: measured facts. Cite them; do not rediscover them.
5. `docs/program/decisions.md`: what the owner decided, and what is still open.

`docs/design/` holds the original drafts. Where they disagree with `docs/program/plan.md`, the plan wins.

## If you are a dispatched worker

- Change only the paths your brief names. Stop and report instead of editing anything else.
- Your acceptance is the scenario or command in your brief, with the values it must print. Run it and paste the
  output in your final report.
- Never run against the Echo, the Ring or a real account. Tiers T0–T4 only.
- Stop and report on: a contract change, a new dependency, a hardware question, or a rule you cannot meet.

## Current phase

Phase F, the repository harness (`docs/program/plan.md` section 6). Nothing else starts before milestone M0.

## Notes

- Codex workers must run from inside a git worktree.
- This repository is public: no secrets, serial numbers, account ids or personal details.
