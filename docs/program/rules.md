---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: active; every change follows these rules
---

# Standing rules

Every change in this repository follows these rules, whoever writes it: the coordinator or a factory worker.
Where a rule can be checked by a machine, Phase F turns it into a required CI check (plan section 11). Until that
check exists, the coordinator checks it by hand.

## Order of work

1. **Gates before code.** No crate and no product code until milestone M0 passes: every Phase F gate
   runs in CI and has been seen to fail on a seeded violation.
2. **Contracts before implementations.** `echo-contracts`, `echo-screen-contract` and `echo-agent-api` change only in a
   change of their
   own, made by the coordinator, never as a side effect of a feature.
3. **Simulated before physical.** A milestone is attempted on the Echo only after its scenario passes with
   `--driver sim` against `hassmic-host` (and `--driver cloud` where it involves a service).

## Code

- **Strict from the first commit.** There is no baseline file. Adding one, an `#[allow]`, a `// NOLINT` or a
  lowered threshold is a rule violation unless `docs/program/decisions.md` records why.
- **Rust:** `unsafe` only in crates `lanes.toml` marks `ffi`, each block with a `// SAFETY:` comment. No `unwrap`,
  `expect`,
  `panic!`, `todo!`, `dbg!` or `println!` outside tests and `xtask`. Functions of at most 60 lines.
- **Shape:** files of at most 500 lines; at most 10 files per module directory, tests included; the crate set
  equals the list in `lanes.toml`.
- **Layering:** `echo-contracts`, `echo-config` and `echo-screen-contract` have no network, hardware or
  async-runtime dependencies. `echo-wyoming`, `echo-audio` and `echo-live` never depend on `agent-*` or `mcp-*`.
  Port OS OAuth types live only in `echo-mcp-auth`; the ChatGPT sign-in lives only in `echo-chatgpt-auth`.
  Only `echo-mcp-auth` may depend on OAuth packages (`oauth2`, `openidconnect`).
- **No C of ours.** The Echo runs echo-dot-assist's `hassmic` at a pinned commit. A change it needs goes upstream
  as a pull request, never as a patch carried here.
- **Scripts and the screen page:** `shellcheck` and `shfmt` for `scripts/`; `prettier`, `eslint` and
  `tsc --noEmit` for `web/screen/`.
- **Dependencies:** a new crate in `Cargo.lock` needs an ADR in `docs/adr/` named in the pull request.
  `cargo-deny` checks licenses and advisories. Code taken from echo-dot-assist (MIT) keeps its notice.

## Tests

- **Behavior, not presence.** Assert values: the call count is 1, the delegation id matches, the spoken reply
  contains the phrase. `len(x) > 0` and "no error" are not acceptance.
- **Tiers are enforced.** T0–T2 never touch the network. T3 uses only local fakes. T4 uses the Port OS pilot with
  simulator devices and a test principal. T5 is the operator-run hardware bench (plan section 8.1).
- **One scenario, many drivers.** A milestone's scenario file is the same for `sim`, `cloud` and `device`; only the
  driver changes.
- **Component T3 tests may run without echo-lab.** A crate's own socket or process tests (a fake peer, a mock
  OAuth server, the pinned `hassmic-host`) live in the crate and run under `make test-unit` or `make test-sim`. Each
  is named or documented as T3, uses only 127.0.0.1 and local processes, waits on readiness rather than fixed sleeps,
  and stays inside nextest's 30 s timeout with no retries. They prove a piece; they never stand in for a milestone
  scenario, which always runs through `echo-lab run` with one file for every driver. Network-free tests stay T1/T2.
- **Coverage floors hold.** A change that drops a crate below its floor (plan section 11) fails `coverage`.

## Secrets and privacy

- No credential in the repository, in logs, in traces or in test fixtures. `gitleaks` runs in CI.
- Types that hold secrets wrap them in `Secret<T>`, which implements neither `Debug` nor `Display`.
- No microphone audio is kept by default. Bench recordings stay in the operator's artifact directory, never in
  git.
- No device serial numbers, account ids, home network details or personal names in the repository. This
  repository is public.

## Hardware

- Only the coordinator runs against the Echo, the Ring or real accounts, and only when the operator has started a
  T5 run (`docs/program/coordinator.md`). Factory workers run tiers T0–T4 only.
- Installing or uninstalling hassmic on the Echo writes `/system`; it needs the owner's go-ahead each time.
- No audio on household devices before 09:00 local time; check the clock right before each run.
- Never change partitions, the bootloader or firmware without the operator's explicit go-ahead.
- The `device` driver does not run against a firmware build that has not been probed (`docs/program/baselines.md`
  lists the probed builds).

## Pull requests

- One change per pull request, named for the plan item it delivers.
- The description says what was run and what it printed. Evidence of a CI run goes in a comment, not a commit.
- Merge with `gh pr merge --squash` only when every required check is green. Never `--admin`.
- Generated files are regenerated, never hand-merged.

## Documents

- Documents in `docs/` start with frontmatter: `author`, `owner`, `last modified`, `status`.
- A change that adds a page under `docs/` (an ADR, a spike report) also adds that page's own link to `docs/README.md`
  in the same change, since the docs index gate requires it. A factory lane may make that one-line index edit even
  though `docs/README.md` is not in its payload's paths; nothing else outside its paths.
- `docs/program/plan.md` wins over the drafts in `docs/design/`. Update the plan in the same change that departs
  from it.
- Every owner request and decision goes into `docs/program/decisions.md` in the change that acts on it.
