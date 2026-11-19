"""T0 regressions for the documentation checker's public checks and launchers."""

from contextlib import redirect_stdout
import io
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import lint_docs as lint


HEADER = "---\nauthor: Team\nowner: Team\nlast modified: today\nstatus: active\n---\n"


def config_fixture(root):
    directory = root / "crates/echo-config/src"
    directory.mkdir(parents=True)
    (directory / "lib.rs").write_text("mod config_decode;\nmod config_settings;\n", encoding="utf-8")
    source = '''fn decode(value: Value) {
    let mut f = Fields::new(value, "voice")?;
    let seconds = f.optional("session_listen_seconds")?;
    f.finish()?;
}'''
    (directory / "config_decode.rs").write_text(source, encoding="utf-8")
    (directory / "config_settings.rs").write_text(
        'let mut f = Fields::new(value, "voice")?; f.finish()?;', encoding="utf-8")
    rows = "| `voice` | Section |\n| `voice.session_listen_seconds` | Seconds |\n"
    (root / "docs/configuration.md").write_text(rows, encoding="utf-8")
    return directory


class ConfigCoverageTests(unittest.TestCase):
    def test_omission_fails_main_then_restoration_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "docs").mkdir()
            config_fixture(root)
            (root / "docs/README.md").write_text(HEADER + "\n[Self](README.md)\n", encoding="utf-8")
            page = root / "docs/configuration.md"
            complete = page.read_text(encoding="utf-8")
            page.write_text("| `voice` | Section |\n", encoding="utf-8")
            diagnostic = "docs/configuration.md: undocumented config keys: voice.session_listen_seconds"
            from contextlib import redirect_stderr
            errors = io.StringIO()
            with patch.object(lint.sys, "argv", ["lint_docs.py"]), \
                    patch.object(lint.Path, "cwd", return_value=root), \
                    patch.object(lint, "manifest", return_value=["docs/README.md"]), \
                    patch.object(lint, "run"), \
                    redirect_stderr(errors):
                self.assertEqual(lint.main(), 1)
                self.assertEqual(errors.getvalue(), "lint-docs: " + diagnostic + "\n")
                print(errors.getvalue().strip())
                page.write_text(complete, encoding="utf-8")
                self.assertEqual(lint.main(), 0)

    def test_mentions_and_other_sections_do_not_cover_missing_row(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "docs").mkdir()
            config_fixture(root)
            (root / "docs/configuration.md").write_text(
                "| `voice` | Section |\nvoice.session_listen_seconds\n"
                "<!--\n| `voice.session_listen_seconds` | Comment |\n-->\n"
                "~~~md\n| `voice.session_listen_seconds` | Example |\n~~~\n"
                "| `live.session_listen_seconds` | Other |\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "keys: voice.session_listen_seconds$"):
                lint.config_coverage(root)

    def test_production_paths_and_new_synthetic_key(self):
        root = Path(__file__).resolve().parent.parent
        keys = lint.accepted_config_keys(root)
        self.assertTrue({"wake_earcon", "home.agent_tools", "codex.delegate_effort",
                         "mcp.server[]", "mcp.server[].allow_tools"} <= keys)
        self.assertFalse({"codex.instructions", "codex.live_instructions"} & keys)
        source = 'let mut f = Fields::new(value, "voice")?; f.optional("future")?; f.finish()?;'
        self.assertEqual(lint.decoder_keys(source, "synthetic"), {"voice", "voice.future"})
        with tempfile.TemporaryDirectory() as directory:
            temp = Path(directory)
            (temp / "docs").mkdir()
            inputs = config_fixture(temp)
            (inputs / "config_decode.rs").write_text(source, encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "keys: voice.future$"):
                lint.config_coverage(temp)
            with (temp / "docs/configuration.md").open("a", encoding="utf-8") as page:
                page.write("| `voice.future` | Future |\n")
            lint.config_coverage(temp)

    def test_unsupported_or_missing_inputs_fail_closed(self):
        for source in ["fn decode(value: Value) {}", 'let mut f = Fields::new(value, path)?;',
                       'let mut f = Fields::new(value, "voice")?; f.optional(KEY)?; f.finish()?;',
                       'let mut f = Fields::new(value, "voice")?; f.optional::<bool>("new")?; f.finish()?;',
                       'let mut f = Fields::new(value, "voice")?; let x = f.section("new")?; f.finish()?;',
                       'let mut f = Fields::new(value, "voice")?; f.new_method("key")?; f.finish()?;']:
            with self.subTest(source=source), self.assertRaisesRegex(ValueError, "config coverage:"):
                lint.decoder_keys(source, "synthetic")
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(OSError):
                lint.accepted_config_keys(Path(directory))
            root = Path(directory)
            (root / "docs").mkdir()
            inputs = config_fixture(root)
            (inputs / "config_settings.rs").write_text("", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "empty or unsupported decoder"):
                lint.accepted_config_keys(root)
            (inputs / "config_decode.rs").unlink()
            with self.assertRaises(OSError):
                lint.accepted_config_keys(root)

    def test_overlay_and_sorted_diagnostics(self):
        source = '''let mut fields = Fields::new(value, "config")?;
        if let Some(mut agent) = fields.section("agent")? {
            agent.optional("max_tool_calls")?; agent.finish()?;
        } fields.finish()?;'''
        self.assertEqual(lint.decoder_keys(source, "overlay"), {"agent", "agent.max_tool_calls"})
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "docs").mkdir()
            inputs = config_fixture(root)
            page = root / "docs/configuration.md"
            page.write_text("", encoding="utf-8")
            for names in [("z", "a"), ("a", "z")]:
                body = 'let mut f = Fields::new(value, "voice")?;'
                body += ''.join(f'f.optional("{key}")?;' for key in names) + 'f.finish()?;'
                (inputs / "config_decode.rs").write_text(body, encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "keys: voice, voice.a, voice.z$"):
                    lint.config_coverage(root)

    def test_receiver_escapes_fail_main_then_restoration_passes(self):
        from contextlib import redirect_stderr
        helper = '''
fn read_extra(fields: &mut Fields<'_>) -> Result<(), ConfigError> {
    let _future: Option<bool> = fields.optional("future")?;
    Ok(())
}'''
        cases = [
            ('read_extra(&mut f)?;', 'f'),
            ('read_extra(&f)?;', 'f'),
            ('read_extra(f)?;', 'f'),
            ('let other = &mut f; read_extra(other)?;', 'f'),
            ('let read = || read_extra(&mut f);', 'f'),
            ('read_table(&mut f.table)?;', 'f'),
            ('if let Some(mut child) = f.section("child")? { '
             'read_extra(&mut child)?; child.finish()?; }', 'child'),
        ]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "docs").mkdir()
            inputs = config_fixture(root)
            decoder = inputs / "config_decode.rs"
            original = decoder.read_text(encoding="utf-8")
            with patch.object(lint.sys, "argv", ["lint_docs.py"]), \
                    patch.object(lint.Path, "cwd", return_value=root), \
                    patch.object(lint, "manifest", return_value=[]), \
                    patch.object(lint, "check_index"), patch.object(lint, "run"):
                for escape, receiver in cases:
                    with self.subTest(escape=escape):
                        decoder.write_text(original.replace('f.finish()?;', escape + ' f.finish()?;')
                                           + helper, encoding="utf-8")
                        errors, output = io.StringIO(), io.StringIO()
                        with redirect_stderr(errors), redirect_stdout(output):
                            self.assertEqual(lint.main(), 1)
                        self.assertEqual(errors.getvalue(),
                                         f"lint-docs: config coverage: unsupported Fields receiver use ({receiver}) "
                                         "in config_decode.rs; helper calls, aliases and captures "
                                         "require decoder reader support\n")
                        self.assertNotIn("Config coverage:", output.getvalue())
                        self.assertNotIn("lint-docs: passed", output.getvalue())
                decoder.write_text(original, encoding="utf-8")
                with redirect_stdout(io.StringIO()) as output:
                    self.assertEqual(lint.main(), 0)
                self.assertIn("Config coverage: 2 accepted paths documented", output.getvalue())
                self.assertIn("lint-docs: passed", output.getvalue())


class MetadataTests(unittest.TestCase):
    def test_required_scalars(self):
        lint.frontmatter("docs/a.md", HEADER)
        lint.frontmatter("docs/a.md", HEADER.replace("Team", "'Team'"))
        cases = [
            ("# No header", "frontmatter requires"),
            (HEADER.replace("status: active\n", ""), "missing frontmatter fields status"),
            (HEADER.replace("status: active", "status: ''"), "empty frontmatter field status"),
            (HEADER.replace("status: active", "status: # empty"), "empty frontmatter field status"),
            (HEADER[:-4], "unterminated"),
            (HEADER.replace("status: active", "status: active\nstatus: other"), "duplicate"),
            (HEADER.replace("status: active", "status: [active]"), "must be a scalar"),
            (HEADER.replace("status: active", "status: 'active"), "malformed quoted"),
            (HEADER.replace("status: active", " status: active"), "malformed top-level"),
        ]
        for text, diagnostic in cases:
            with self.subTest(diagnostic=diagnostic), self.assertRaisesRegex(ValueError, diagnostic):
                lint.frontmatter("docs/a.md", text)

    def test_index_resolves_escaped_paths_and_references(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "docs").mkdir()
            index = root / "docs/README.md"
            index.write_text("[Self](./README.md)\n[Page][p]\n[p]: a%20b.md#heading\n", encoding="utf-8")
            lint.check_index(root, ["docs/README.md", "docs/a b.md"])
            with self.assertRaisesRegex(ValueError, "docs/missing.md"):
                lint.check_index(root, ["docs/missing.md"])

    def test_manifest_only_uses_git_tracked_paths(self):
        result = subprocess.CompletedProcess([], 0, b"docs/a b.md\0docs/other.txt\0README.md\0")
        with patch.object(lint, "run", return_value=result) as launch:
            self.assertEqual(lint.manifest(Path(".")), ["docs/a b.md", "README.md"])
            self.assertEqual(launch.call_args.args[0], ["git", "ls-files", "-z"])
        self.assertEqual(lint.scoped(["factory/a.md", "docs/a.md", "AGENTS.md", "CLAUDE.md"]),
                         ["docs/a.md", "AGENTS.md"])


class AdrTests(unittest.TestCase):
    def test_valid_names_and_readme(self):
        self.assertEqual(lint.adr_findings([
            ("0000-a.md", "# 0000: A"), ("0001-b.md", "# B"),
            ("README.md", "# 9999: Index")]), [])

    def test_duplicate_pair_sorted(self):
        self.assertEqual(lint.adr_findings([("0003-b.md", ""), ("0003-a.md", "")]),
                         ["docs/adr: ADR number 0003 used by 0003-a.md, 0003-b.md"])

    def test_duplicate_triple(self):
        self.assertEqual(lint.adr_findings([
            ("0005-c.md", ""), ("0005-a.md", ""), ("0005-b.md", "")]),
            ["docs/adr: ADR number 0005 used by 0005-a.md, 0005-b.md, 0005-c.md"])

    def test_malformed_names(self):
        self.assertEqual(lint.adr_findings([
            ("el-f4-thing.md", ""), ("12-x.md", ""), ("0004-Upper.md", "")]), [
                "docs/adr/0004-Upper.md: ADR file name must be NNNN-<slug>.md",
                "docs/adr/12-x.md: ADR file name must be NNNN-<slug>.md",
                "docs/adr/el-f4-thing.md: ADR file name must be NNNN-<slug>.md"])

    def test_heading_mismatch(self):
        self.assertEqual(lint.adr_findings([("0007-x.md", HEADER + "# 0005: Title")]),
                         ["docs/adr/0007-x.md: heading number 0005 does not match file number 0007"])

    def test_only_first_literal_heading_counts(self):
        for text in ["# 0007: Title", "# Title\n# 0005: Later", "## 0005: Sub\n# 0007: Title",
                     " # 0005: Indented\n# Title", "# 0005 Title", "# 00005: Title",
                     "# ０００５: Title"]:
            with self.subTest(text=text):
                self.assertEqual(lint.adr_findings([("0007-x.md", text)]), [])

    def test_boundaries(self):
        self.assertEqual(lint.adr_findings([]), [])
        self.assertEqual(lint.adr_findings([("0000-a.md", "no heading"), ("9999-z9.md", "")]), [])
        for name in ["0004-.md", "00004-x.md", "0004-x.txt", "０００４-x.md", "readme.md"]:
            with self.subTest(name=name):
                self.assertEqual(lint.adr_findings([(name, "# 0005: Ignored")]),
                                 [f"docs/adr/{name}: ADR file name must be NNNN-<slug>.md"])

    def test_all_findings_collected(self):
        files = [(name, "") for name in ["0005-c.md", "0003-b.md", "0005-a.md", "0003-a.md",
                                        "0005-b.md", "el-f4-thing.md", "12-x.md", "0004-Upper.md"]]
        files += [("0007-x.md", "# 0005: X"), ("0008-y.md", "# 0006: Y")]
        self.assertEqual(lint.adr_findings(files), [
            "docs/adr/0004-Upper.md: ADR file name must be NNNN-<slug>.md",
            "docs/adr/0007-x.md: heading number 0005 does not match file number 0007",
            "docs/adr/0008-y.md: heading number 0006 does not match file number 0008",
            "docs/adr/12-x.md: ADR file name must be NNNN-<slug>.md",
            "docs/adr/el-f4-thing.md: ADR file name must be NNNN-<slug>.md",
            "docs/adr: ADR number 0003 used by 0003-a.md, 0003-b.md",
            "docs/adr: ADR number 0005 used by 0005-a.md, 0005-b.md, 0005-c.md"])


class AdrDirectoryTests(unittest.TestCase):
    def check_directory(self, files, expected_error=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            adr = root / "docs/adr"
            adr.mkdir(parents=True)
            (adr / "nested").mkdir()
            (adr / "nested/invalid.md").write_text("", encoding="utf-8")
            for name, text in files:
                (adr / name).write_text(text, encoding="utf-8")
            (root / "docs/README.md").write_text(
                HEADER + "\n[Self](README.md)\n", encoding="utf-8")
            config_fixture(root)
            output = io.StringIO()
            with patch.object(lint, "manifest", return_value=["docs/README.md"]), \
                    patch.object(lint, "run") as launch, redirect_stdout(output):
                if expected_error:
                    with self.assertRaises(ValueError) as caught:
                        lint.check(root)
                    self.assertEqual(str(caught.exception), expected_error)
                    launch.assert_not_called()
                else:
                    lint.check(root)
                    self.assertEqual([call.args[0] for call in launch.call_args_list], [
                        ["npx", "--yes", lint.MARKDOWNLINT, "docs/README.md"],
                        ["lychee", "--config", "lychee.toml", "--offline", "docs/README.md"]])
                    self.assertEqual(output.getvalue().splitlines()[-1], "lint-docs: passed")

    def test_valid_directory_reaches_tools(self):
        self.check_directory([("0003-a.md", "# 0003: A"), ("README.md", "# Index")])

    def test_untracked_duplicate_stops_before_tools(self):
        self.check_directory([("0003-a.md", ""), ("0003-b.md", "")],
                             "docs/adr: ADR number 0003 used by 0003-a.md, 0003-b.md")

    def test_non_markdown_file_is_not_filtered_out(self):
        self.check_directory([("notes.txt", "")],
                             "docs/adr/notes.txt: ADR file name must be NNNN-<slug>.md")


class FenceTests(unittest.TestCase):
    def test_nested_example_and_real_fence(self):
        text = "````markdown\n```mermaid\nexample\n```\n````\n~~~mermaid\nreal\n~~~\n"
        self.assertEqual(lint.fences("a.md", text), [(6, "real\n")])

    def test_short_or_different_closing_fence_is_body(self):
        self.assertEqual(lint.fences("a.md", "````mermaid\n```\n~~~\n````\n"),
                         [(1, "```\n~~~\n")])

    def test_empty_and_unterminated(self):
        self.assertEqual(lint.fences("a.md", "# No diagrams\n"), [])
        with self.assertRaisesRegex(ValueError, "a.md:2: unclosed"):
            lint.fences("a.md", "hello\n```mermaid\na --> b\n")


class StyleTests(unittest.TestCase):
    def test_real_pinned_style_boundaries(self):
        config = Path(__file__).resolve().parent.parent / ".markdownlint-cli2.jsonc"
        table = "# Table\n\n| Column |\n| --- |\n| " + "x" * 130 + " |\n"
        cases = [(table, None), ("# Long\n\n" + "word " * 30 + "\n", "MD013"),
                 (table.replace("| --- |", "|---|"), "MD060")]
        with tempfile.TemporaryDirectory() as directory:
            page = Path(directory) / "style.md"
            for body, expected in cases:
                page.write_text(body, encoding="utf-8")
                with patch.dict(lint.os.environ, {"npm_config_offline": "true"}):
                    result = subprocess.run(
                        [lint.shutil.which("npx"), "--yes", lint.MARKDOWNLINT,
                         "--config", str(config), str(page)],
                        capture_output=True, text=True, check=False)
                if expected:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(expected, result.stdout + result.stderr)
                else:
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


class LauncherTests(unittest.TestCase):
    def test_missing_tool_is_actionable(self):
        with patch.object(lint.shutil, "which", return_value=None):
            with self.assertRaisesRegex(ValueError, "required tool lychee is missing"):
                lint.run(["lychee"])

    def test_nonzero_tool_failure(self):
        with patch.object(lint.shutil, "which", return_value="tool"), \
                patch.object(lint.subprocess, "run", return_value=subprocess.CompletedProcess([], 7)):
            with self.assertRaisesRegex(ValueError, "exit 7"):
                lint.run(["tool"])

    def check_fixture(self, online=False, failure=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "docs").mkdir()
            (root / "docs/README.md").write_text(HEADER + "\n# Docs\n\n[Self](README.md)\n", encoding="utf-8")
            config_fixture(root)
            launches = []
            def record(args, **kwargs):
                launches.append(args)
                if failure and args[0] == failure:
                    raise ValueError("controlled tool failure")
                return subprocess.CompletedProcess(args, 0)
            with patch.object(lint, "manifest", return_value=["docs/README.md"]), \
                    patch.object(lint, "run", side_effect=record), redirect_stdout(io.StringIO()):
                lint.check(root, online=online)
            return launches

    def test_offline_and_explicit_online_arguments(self):
        for online in [False, True]:
            calls = self.check_fixture(online)
            lychee = next(call for call in calls if call[0] == "lychee")
            self.assertEqual("--offline" in lychee, not online)
            self.assertEqual(lychee[1:3], ["--config", "lychee.toml"])
            self.assertEqual(calls[0], ["npx", "--yes", lint.MARKDOWNLINT, "docs/README.md"])

    def test_root_links_are_checked_and_vendored_links_excluded(self):
        # Real offline lychee, isolated files; style/browser launchers are controlled.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "docs").mkdir()
            (root / "factory").mkdir()
            (root / "docs/README.md").write_text(
                HEADER + "\n# Docs\n\n[Self](README.md)\n", encoding="utf-8")
            page = root / "NOTES with spaces.md"
            page.write_text("# Notes\n\n[Target](missing.md)\n", encoding="utf-8")
            (root / "factory/vendor.md").write_text(
                "# Vendor\n\n[Broken](absent.md)\n", encoding="utf-8")
            (root / "lychee.toml").write_text('include_fragments = "full"\n', encoding="utf-8")
            paths = ["docs/README.md", "NOTES with spaces.md", "factory/vendor.md"]
            config_fixture(root)
            launch = lint.run
            results = []
            def local_run(args, **kwargs):
                if args[0] != "lychee":
                    return subprocess.CompletedProcess(args, 0)
                self.assertEqual(args[-2:], paths[:2])
                self.assertIn("--offline", args)
                try:
                    return launch(args, capture_output=True, **kwargs)
                except ValueError as error:
                    results.append(str(error))
                    raise
            with patch.object(lint, "manifest", return_value=paths), \
                    patch.object(lint, "run", side_effect=local_run), redirect_stdout(io.StringIO()):
                with self.assertRaisesRegex(ValueError, "lychee failed"):
                    lint.check(root)
                page.write_text("# Notes\n\n[Target](docs/README.md#docs)\n", encoding="utf-8")
                lint.check(root)
            self.assertEqual(len(results), 1)

    def test_unknown_mode_fails_before_checks(self):
        with patch.object(lint.sys, "argv", ["lint_docs.py", "--unknown"]):
            self.assertEqual(lint.main(), 2)

    def test_unreadable_input_and_temp_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(lint, "manifest", return_value=["docs/missing.md"]):
                with self.assertRaises(FileNotFoundError):
                    lint.check(Path(directory))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "docs").mkdir()
            (root / "docs/README.md").write_text(HEADER + "\n[Self](README.md)\n", encoding="utf-8")
            config_fixture(root)
            with patch.object(lint, "manifest", return_value=["docs/README.md"]), \
                    patch.object(lint, "run"), \
                    patch.object(lint.tempfile, "TemporaryDirectory", side_effect=OSError("temp unavailable")):
                with self.assertRaisesRegex(OSError, "temp unavailable"):
                    lint.check(root)

    def test_render_failure_names_source_and_cleans_temp(self):
        parents = []
        def render(args, **kwargs):
            source = Path(args[args.index("-i") + 1])
            parents.append(source.parent)
            raise ValueError("browser cannot start")
        for _ in range(2):
            with tempfile.TemporaryDirectory() as directory:
                with patch.object(lint, "run", side_effect=render):
                    with self.assertRaisesRegex(ValueError, "docs/a.md:12: Mermaid compile failed"):
                        lint.compile_mermaid([("docs/a.md", 12, "a --> b")], Path(directory))
            self.assertFalse(parents[-1].exists())
        self.assertNotEqual(parents[0], parents[1])

    def test_render_requires_fresh_nonempty_svg(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(lint, "run"):
            with self.assertRaisesRegex(ValueError, "produced no SVG"):
                lint.compile_mermaid([("a.md", 1, "graph LR\na --> b")], Path(directory))


if __name__ == "__main__":
    unittest.main()
