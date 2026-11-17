"""Network-free factory component tests: real CLI journeys and isolated guards."""

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("setup-workspace.py").resolve()
spec = importlib.util.spec_from_file_location("detached_setup", SCRIPT)
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)
LANE = "fixture-lane"
PACKET = b'{"project":"fixture","userStories":[]}\n'
MARKDOWN = b"# Fixture\n"


class DetachedPlannerCliTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "repo"
        self.root.mkdir()
        self.git("init", "-b", "main")
        self.git("config", "user.name", "Fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        (self.root / ".gitignore").write_text(".claude/\ntasks/\nprd.*\nprogress.txt\n")
        self.git("add", ".gitignore")
        self.git("commit", "-m", "Fixture base")
        self.base = self.git("rev-parse", "HEAD").stdout.strip()
        self.destination = self.root / ".claude/worktrees" / LANE

    def git(self, *args, cwd=None, check=True):
        return subprocess.run(
            ["git", *args], cwd=cwd or self.root, text=True,
            capture_output=True, check=check, timeout=30,
        )

    def commit(self, cwd=None):
        self.git("commit", "--allow-empty", "-m", "Fixture advance", cwd=cwd)
        return self.git("rev-parse", "HEAD", cwd=cwd).stdout.strip()

    def planner(self, name=f"{LANE}-plan", branch=None):
        self.source = self.root / ".claude/worktrees" / name
        args = ["-b", branch] if branch else ["--detach"]
        self.git("worktree", "add", *args, str(self.source), "main")
        self.source_head = self.git("rev-parse", "HEAD", cwd=self.source).stdout.strip()
        self.packet = self.source / "tasks/todo" / f"{LANE}.json"
        self.packet.parent.mkdir(parents=True)
        self.packet.write_bytes(PACKET)
        self.packet.with_suffix(".md").write_bytes(MARKDOWN)

    def origin(self):
        bare = Path(self.temp.name) / "origin.git"
        self.git("init", "--bare", str(bare))
        self.git("remote", "add", "origin", str(bare))
        self.git("push", "origin", "main")

    def run_cli(self):
        return subprocess.run(
            [sys.executable, str(SCRIPT), LANE], cwd=self.root,
            text=True, capture_output=True, timeout=30,
        )

    def assert_source(self, branch=None):
        self.assertEqual(self.git("rev-parse", "HEAD", cwd=self.source).stdout.strip(), self.source_head)
        symbolic = self.git("symbolic-ref", "--quiet", "HEAD", cwd=self.source, check=False)
        self.assertEqual(symbolic.returncode, 0 if branch else 1)
        if branch:
            self.assertEqual(symbolic.stdout.strip(), f"refs/heads/{branch}")
        self.assertEqual(self.packet.read_bytes(), PACKET)
        self.assertEqual(self.packet.with_suffix(".md").read_bytes(), MARKDOWN)

    def assert_ready(self):
        result = self.run_cli()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), {
            "status": "ready", "worktree": str(self.destination), "branch": LANE,
            "prd_path": str(self.destination / "prd.json"),
            "prd_md_path": str(self.destination / "prd.md"),
            "standing_rules_path": None, "reused": False,
        })
        self.assertEqual(self.git("symbolic-ref", "HEAD", cwd=self.destination).stdout.strip(), f"refs/heads/{LANE}")
        self.assertEqual(self.git("rev-parse", "HEAD", cwd=self.destination).stdout.strip(), self.git("rev-parse", "main").stdout.strip())
        self.assertEqual((self.destination / "prd.json").read_bytes(), PACKET)
        self.assertEqual((self.destination / "prd.md").read_bytes(), MARKDOWN)
        self.assert_source()

    def assert_refused(self, reason, branch=None):
        result = self.run_cli()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertIn(reason, result.stderr)
        self.assertFalse(self.destination.exists())
        self.assertNotEqual(self.git("show-ref", "--verify", f"refs/heads/{LANE}", check=False).returncode, 0)
        self.assert_source(branch)

    def test_p1_equal_main_without_origin(self):
        self.planner()
        self.assert_ready()

    def test_p2_ancestor_of_main(self):
        self.planner()
        self.commit()
        self.assert_ready()

    def test_p3_other_detached_name(self):
        self.planner("other-plan")
        self.assert_refused('branch is "detached"')

    def test_p4_unreachable_exact_planner(self):
        self.planner()
        self.source_head = self.commit(self.source)
        self.assert_refused("ancestry check failed")

    def test_p5_conflicting_root_packet(self):
        self.planner()
        root_packet = self.root / "tasks/todo" / f"{LANE}.json"
        root_packet.parent.mkdir(parents=True)
        root_packet.write_text('{"project":"different"}')
        self.assert_refused("ambiguous PRD")

    def test_p6_local_origin(self):
        self.origin()
        self.planner()
        self.assert_ready()

    def test_p7_foreign_attached_branch(self):
        self.planner(branch="foreign")
        self.assert_refused("refs/heads/foreign", branch="foreign")

    def test_p8_origin_main_is_authoritative(self):
        self.origin()
        self.commit()
        self.planner()
        self.assert_refused("ancestry check failed")


class DetachedPlannerGuardTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / f"{LANE}-plan"
        self.source.mkdir()
        (self.source / ".git").touch()
        self.candidate = {
            "worktree_path": self.source,
            "prd_json_path": self.source / f"{LANE}.json",
            "record": {"locked": False, "prunable": False, "detached": True, "branch_ref": None},
        }

    def validate(self):
        setup.validate_nested_packet_freshness(self.root, self.candidate, LANE)

    def test_g1_registration_guards(self):
        cases = [({"locked": True}, "locked"), ({"prunable": True}, "prunable"),
                 ({"detached": False, "branch_ref": "refs/heads/foreign"}, "foreign"),
                 ({"detached": False}, "missing")]
        for changes, reason in cases:
            with self.subTest(changes=changes), patch.dict(self.candidate["record"], changes):
                with self.assertRaisesRegex(RuntimeError, reason):
                    self.validate()
        self.candidate["worktree_path"] = self.root / "other-plan"
        with self.assertRaisesRegex(RuntimeError, "detached"):
            self.validate()

    def test_g2_missing_git(self):
        (self.source / ".git").unlink()
        with self.assertRaisesRegex(RuntimeError, "unavailable"):
            self.validate()

    def test_g3_baseline_and_head_fail_closed(self):
        for origin in (False, True):
            expected = "refs/remotes/origin/main" if origin else "refs/heads/main"
            with self.subTest(origin=origin), patch.object(setup, "has_origin_remote", return_value=origin):
                with patch.object(setup, "resolve_revision_head", side_effect=RuntimeError("missing baseline")) as resolve:
                    with self.assertRaisesRegex(RuntimeError, "missing baseline"):
                        self.validate()
                    resolve.assert_called_once_with(self.root, expected, "planner baseline")
                with patch.object(setup, "resolve_revision_head", return_value="a" * 40):
                    with patch.object(setup, "resolve_checkout_head", return_value=None):
                        with self.assertRaisesRegex(RuntimeError, "readable HEAD"):
                            self.validate()

    def test_g4_failed_ancestry_probe(self):
        with patch.object(setup, "has_origin_remote", return_value=False), \
             patch.object(setup, "resolve_revision_head", return_value="a" * 40), \
             patch.object(setup, "resolve_checkout_head", return_value="b" * 40), \
             patch.object(setup, "run_git", return_value=subprocess.CompletedProcess([], 128, "", "probe failed")) as git:
            with self.assertRaisesRegex(RuntimeError, "ancestry check failed"):
                self.validate()
            git.assert_called_once_with("merge-base", "--is-ancestor", "b" * 40, "a" * 40, cwd=self.root, check=False)

    def test_g5_branch_hint_still_checked(self):
        self.candidate["is_root"] = False
        with patch.object(setup, "packet_candidates", return_value=[self.candidate]), \
             patch.object(setup, "read_prd", return_value={"branchName": "foreign"}):
            with self.assertRaisesRegex(ValueError, "branchName mismatch"):
                setup.select_prd_candidate(self.root, LANE, [])

    def test_g6_attached_freshness_unchanged(self):
        self.candidate["record"].update(detached=False, branch_ref=f"refs/heads/{LANE}")
        for ahead, remote, accepted in ((1, False, True), (0, False, False), (0, True, True)):
            with self.subTest(ahead=ahead, remote=remote), \
                 patch.object(setup, "branch_exists_on_remote", return_value=remote), \
                 patch.object(setup, "origin_main_ref_exists", return_value=True), \
                 patch.object(setup, "run_git", return_value=subprocess.CompletedProcess([], 0, str(ahead), "")):
                if accepted:
                    self.validate()
                else:
                    with self.assertRaisesRegex(RuntimeError, "appears abandoned"):
                        self.validate()


if __name__ == "__main__":
    unittest.main()
