"""T0 checks of the worker-visible process instruction contract; no execution."""

from pathlib import Path
import re
import unittest


PROMPT = Path(__file__).resolve().parents[1] / "workstations/process/AGENTS.md"

# Inert retired paragraph: never executed or offered as a supported route.
OLD_LOCAL_HOST = """- Local Linux host: `pe-ci` is the only supported local WSL distro; hosted Ubuntu is preferred. Never use,
  start, repair or wait for any other distro (the default `Ubuntu` distro is
  unavailable). Run Linux-side witnesses (pinned `hassmic-host`, T3 tests, sim
  scenarios) on hosted targeted runs; authorized hosted-unsupported work inside pe-ci: `wsl -d pe-ci -u root -- sh <script>.sh`
  from a script file with LF line endings that sources `/opt/pe-ci-env.sh`,
  holds `/var/tmp/pe-ci.lock` for every build or test, and builds into its own
  `/var/tmp` target deleted by an EXIT trap. Inside pe-ci, git cannot follow a
  worktree's absolute Windows `gitdir: C:/...` link. Rewrite your worktree's
  `.git` file to the relative form `gitdir: ../../../.git/worktrees/<name>`
  (Windows git reads it too) and pass `-c safe.directory='*'`. Never export
  `GIT_DIR` or `GIT_WORK_TREE` in a script: `scripts/hassmic-host.sh` runs its
  own `git init`, `fetch`, `sparse-checkout` and `checkout`, and an exported
  `GIT_DIR` points them at the repository, which rewrites its config and your
  worktree.
  Detached (`setsid`) pe-ci runners lose Windows interop and can die: for a run
  that outlives your visit, write the script, then park naming it so the
  coordinator runs it attached and relays the markers."""

HOSTED = (
    "Linux is the only supported platform for building, testing and running Echo",
    "Lanes dispatch focused/repeated Linux runs themselves through `targeted.yml`, never park for coordinator Linux proof",
    "gh workflow run targeted.yml --ref <lane-branch> -f 'nextest_filter=test(t3_m3_)' -f repeat=2",
    "gh workflow run targeted.yml --ref <lane-branch> -f scenarios=m4-spoof-tool",
    "match run branch/time/head SHA",
    "link the run, inspect TARGETED_RESULT summaries and download scenario report.json artifacts",
    "hosted Ubuntu targeted runs are preferred for focused or repeated tests, pinned `hassmic-host` witnesses and sim scenarios",
)
LOCAL = (
    "`pe-ci` stays coordinator-scheduled for T5 and rare work hosted runners cannot support",
    "A worker may run that rare hosted-unsupported work only when explicitly authorized and coordinator-scheduled",
    "from a Linux shell in a Linux-native checkout or worktree",
    "Use an LF script that sources `/opt/pe-ci-env.sh`",
    "holds `/var/tmp/pe-ci.lock` for every build or test",
    "builds into its own `/var/tmp` target deleted by an EXIT trap",
    "Never export `GIT_DIR` or `GIT_WORK_TREE`",
    "nested Git commands in `scripts/hassmic-host.sh` must select their own repository",
)
HARDWARE = (
    "T5/T6 remain coordinator-only",
    "factory workers never run hardware",
)
DELIVERY = (
    "Quality gate: hosted CI. The required checks on your pushed final head",
    "Do not run a full local `make ci`, and never queue one on the shared pe-ci runner",
    "Required red blocks merge; nextest retries remain zero",
    "Stop after final push, PR open, CI started and feedback addressed; review owns terminal checks, independent validation and merge",
    "Create/ready a non-draft PR",
    "never run `gh pr merge`",
    "For the final head you hand to review, return ACCEPTED once CI has started; ci-wait and review own terminal CI",
)
PROHIBITED = (
    ("Windows launch", r"\bwsl(?:\.exe)?\s+-"),
    ("Windows gitdir", r"gitdir:\s*[a-z]:[/\\]"),
    ("gitdir rewrite", r"rewrite\b[^.]*`\.git`[^.]*relative form"),
    ("safe-directory bypass", r"safe\.directory\s*=\s*['\"]?\*"),
    ("interop parking", r"detached\b.*?windows interop.*?\bpark\b"),
)


def normalized(text):
    return " ".join(text.split())


def assert_required(test, text, clauses):
    text = normalized(text)
    for clause in clauses:
        with test.subTest(clause=clause):
            test.assertTrue(clause in text, f"Missing process obligation: {clause}")


def assert_no_obsolete(test, text, rules=PROHIBITED):
    for label, pattern in rules:
        with test.subTest(directive=label):
            test.assertIsNone(
                re.search(pattern, normalized(text), re.IGNORECASE),
                f"Obsolete process directive: {label}",
            )


class ProcessPlatformGuidanceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.prompt = PROMPT.read_text(encoding="utf-8")

    def test_p1_hosted_linux_route(self):
        assert_required(self, self.prompt, HOSTED)

    def test_p2_rare_local_linux_safeguards(self):
        assert_required(self, self.prompt, LOCAL)

    def test_p3_actual_prompt_has_no_obsolete_directions(self):
        assert_no_obsolete(self, self.prompt)

    def test_p3_retired_paragraph_is_rejected_by_each_rule(self):
        for label, pattern in PROHIBITED:
            with self.subTest(directive=label):
                # A plain TestCase propagates the assertion for assertRaises.
                with self.assertRaisesRegex(AssertionError, label):
                    assert_no_obsolete(unittest.TestCase(), OLD_LOCAL_HOST, ((label, pattern),))

    def test_p4_retirement_statement_is_allowed(self):
        assert_no_obsolete(self, "Windows support is retired; Linux is the only supported platform.")

    def test_p5_hardware_ownership(self):
        assert_required(self, self.prompt, HARDWARE)

    def test_p6_hosted_gate_and_review_delivery(self):
        assert_required(self, self.prompt, DELIVERY)

    def test_p2_p5_p6_missing_obligations_are_rejected(self):
        text = normalized(self.prompt)
        for clause in HOSTED + LOCAL + HARDWARE + DELIVERY:
            with self.subTest(removed=clause):
                self.assertTrue(clause in text, f"Mutation requires the real obligation: {clause}")
                mutant = text.replace(clause, "[obligation removed]")
                with self.assertRaisesRegex(AssertionError, "Missing process obligation"):
                    assert_required(unittest.TestCase(), mutant, (clause,))

    def test_wrapped_prose_is_allowed(self):
        assert_required(self, "\n  ".join(normalized(self.prompt).split()), HOSTED + LOCAL + HARDWARE + DELIVERY)

    def test_empty_prompt_is_rejected(self):
        with self.assertRaisesRegex(AssertionError, "Missing process obligation"):
            assert_required(unittest.TestCase(), "", HOSTED[:1])


if __name__ == "__main__":
    unittest.main(verbosity=2)
