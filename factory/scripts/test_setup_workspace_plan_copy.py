"""Component tests for coalescing a planner's leftover packet copy."""

import importlib.util
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("setup_workspace", Path(__file__).with_name("setup-workspace.py"))
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)

LANE = "el-w0-config"


def candidate(base, worktree, body, is_root):
    path = base / worktree / "tasks" / "todo" / f"{LANE}.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")
    return {"prd_json_path": path, "worktree_path": base / worktree, "is_root": is_root}


class PlannerCopyTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.base = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def test_identical_plan_copy_coalesces_to_root(self):
        root = candidate(self.base, "root", "{}", True)
        copy = candidate(self.base, f"wt/{LANE}-plan", "{}", False)
        self.assertEqual(setup.coalesce_planner_copies([root, copy], LANE), [root])

    def test_differing_plan_copy_stays_ambiguous(self):
        root = candidate(self.base, "root", "{}", True)
        copy = candidate(self.base, f"wt/{LANE}-plan", '{"x": 1}', False)
        self.assertEqual(setup.coalesce_planner_copies([root, copy], LANE), [root, copy])

    def test_copy_in_other_worktree_stays_ambiguous(self):
        root = candidate(self.base, "root", "{}", True)
        copy = candidate(self.base, "wt/el-w0-contracts-plan", "{}", False)
        self.assertEqual(setup.coalesce_planner_copies([root, copy], LANE), [root, copy])

    def test_no_root_candidate_stays_ambiguous(self):
        first = candidate(self.base, f"wt/{LANE}-plan", "{}", False)
        second = candidate(self.base, f"wt2/{LANE}-plan", "{}", False)
        self.assertEqual(setup.coalesce_planner_copies([first, second], LANE), [first, second])

    def test_single_candidate_is_unchanged(self):
        root = candidate(self.base, "root", "{}", True)
        self.assertEqual(setup.coalesce_planner_copies([root], LANE), [root])


if __name__ == "__main__":
    unittest.main()
