# Idea Payload Template (mandatory)

Every `idea` work item submitted by the **ideafy** meta-planner MUST use this
shape. Vague packets cause plan/process agents to invent the wrong system
(for example building a new crate or test subsystem when the ask was only for
a lint or a scenario).

Fill every section. Prefer short bullets over long prose. Do not submit an idea
whose `ask` could be satisfied by two materially different architectures.

## Required Markdown Shape

Use this outline when drafting the idea, then map it into the JSON payload
fields below.

```text
ask:
  <one plain-language paragraph: who cares, what must be true afterward, and
   why this packet exists now>

shape of requirements:
  1. system shape requirements
     - <what may change in package/layout/contracts>
     - <what must stay unchanged>
     - <where work is allowed to live; where it must NOT live>
  2. test coverage requirements
     - <external/public proof expected (CLI/API/events/functional suite)>
     - <reuse existing tests if sufficient, else add only the missing proofs>
     - <explicitly forbid inventing helper subsystems solely for tests>
  3. acceptance criteria
     - <observable outcomes that prove confidence for an external user>
     - <migration/cleanup complete only when those outcomes hold>
```

## Required JSON Payload Fields

Every submitted `idea.payload` MUST include at least:

| Field | Purpose |
|---|---|
| `title` | Short packet title (often includes packet id) |
| `packetId` | Stable packet id when one exists |
| `ask` | Plain-language ask (same content as the Markdown `ask:`) |
| `systemShapeRequirements` | Array of system/layout/contract constraints |
| `testCoverageRequirements` | Array of external proof / coverage constraints |
| `acceptanceCriteria` | Array of reviewer-verifiable outcomes (≥2 behavioral) |
| `outOfScope` | Explicit non-goals and forbidden inventions |
| `antiPatterns` | Concrete wrong implementations to reject |
| `currentProgress` | Why this packet is next (evidence, not slogans) |
| `requestedOutcome` | Single long-form synthesis of ask + requirements + delivery |
| `sourcePath` | Plan/meta source when applicable |

`expectedChangedPaths` may be included as advisory impact analysis. It is not
an allowlist: implementation may add a required path when repository evidence
shows the path belongs to the same behavior and no live branch/worktree
conflict exists. Public-contract and generated-file rules still apply.

Always append the delivery contract to `requestedOutcome` (or as its own final
acceptance bullet): required CI terminal green, blocking PR conversation
comments addressed, merge conflicts reconciled, PR merged, then Factory
complete.

## Writing Rules

1. **External-user first.** Prefer proofs through the public surfaces: an
   `echo-lab` scenario under `tests/scenarios/`, a `make` target, a CLI's
   output, or a crate's public API from `crates/<crate>/tests/`.
2. **Name the proof home.** Say exactly which scenario, test directory or make
   target owns the proof.
3. **Forbid invented scaffolding.** If the ask is "prove X", say explicitly: do
   not add crates, xtasks or test-only subsystems unless the ask lists them. The
   crate list is `lanes.toml` and belongs to the coordinator.
4. **Reuse before invent.** Require validating whether existing tests or
   scenarios already cover the behavior; add tests only for real gaps.
5. **One architecture.** An implementer reading only this payload must not need
   chat context to choose between two materially different designs.
6. **Report impact from reality.** Compare the planned impact with live
   branches/worktrees before implementation and report the final changed paths.
   A missing or incomplete estimate never rejects otherwise valid Work.

## Filled Example (a Phase F gate — correct)

```text
ask:
  Add the shape lint from docs/program/plan.md section 10 (el-f3-shape-lint) so
  that repository shape is enforced by a command, not by reviewers: the crate
  set equals lanes.toml, no file exceeds 500 lines, no module directory holds
  more than 10 files, and names carry their area prefix.

shape of requirements:
  1. system shape requirements
     - The lint is `cargo xtask lint-shape`, wired into `make lint`
     - Do not add or rename crates; lanes.toml is read, never edited here
     - No baseline or allowlist file: every rule is strict from the first commit
  2. test coverage requirements
     - Unit tests for each rule live beside the xtask code (#[cfg(test)])
     - The seeded-violation proof is the PR itself: a commit that adds an
       undeclared crate and a 501-line file shows the lint failing, and a later
       commit removes the seeds
  3. acceptance criteria
     - `make lint` exits non-zero on the seeded undeclared crate and on the
       seeded 501-line file, and the PR shows that output
     - `make lint` exits 0 on the final head, and the PR shows that output
     - PR merged before Factory completion
```

Corresponding JSON fragment:

```json
{
  "title": "el-f3-shape-lint: enforce repository shape with cargo xtask lint-shape",
  "packetId": "el-f3-shape-lint",
  "ask": "Add cargo xtask lint-shape so repository shape is enforced by a command: the crate set equals lanes.toml, files are at most 500 lines, module directories hold at most 10 files, and names carry their area prefix.",
  "systemShapeRequirements": [
    "The lint is cargo xtask lint-shape, wired into make lint",
    "lanes.toml is read, never edited, and no crate is added or renamed",
    "No baseline or allowlist file"
  ],
  "testCoverageRequirements": [
    "Unit tests for each rule beside the xtask code",
    "A seeded-violation commit in the PR shows the lint failing, then is reverted"
  ],
  "acceptanceCriteria": [
    "make lint exits non-zero on a seeded undeclared crate and on a seeded 501-line file, with the output in the PR",
    "make lint exits 0 on the final head, with the output in the PR",
    "PR merged before Factory completion"
  ],
  "outOfScope": [
    "Layering or dependency rules (el-f4-layering)",
    "CI workflows (el-f10-ci)"
  ],
  "antiPatterns": [
    "A baseline file that grandfathers existing violations",
    "Tests that only assert the xtask binary exists",
    "Editing lanes.toml to make the lint pass"
  ],
  "currentProgress": [
    "el-f1-readme-makefile has merged the Makefile and the empty workspace with lanes.toml"
  ],
  "expectedChangedPaths": [
    "xtask/**",
    "Makefile"
  ],
  "requestedOutcome": "Add cargo xtask lint-shape enforcing the crate set, file length, module size and naming rules from docs/program/plan.md section 10, wire it into make lint, prove it with unit tests and a seeded-violation commit whose failing output is in the PR, and remove the seeds. Delivery contract: the quality command passes on the final head, blocking PR conversation comments are addressed, conflicts are reconciled, the PR is merged, and only then complete Factory work."
}
```

## Anti-Example (ambiguous — do not submit)

```text
ask:
  Add the shape checks from the plan.

requestedOutcome:
  Make sure the repository shape is checked, with tests where needed.
```

Why this fails: it names no command, no rule values and no proof, so one lane
writes a runtime test, another a CI script and a third a baseline file. The
correct packet names the command, the exact limits, the seeded-violation proof
and what must not change.
