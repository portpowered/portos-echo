"""Dedicated T0 documentation gate; Python standard library only.

Frontmatter deliberately accepts top-level scalar YAML only: unquoted text or
single/double quoted text, no collections, tags, aliases or block scalars.
"""

import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
from urllib.parse import unquote, urlsplit


VENDORED_PREFIX = "factory/"
REQUIRED = {"author", "owner", "last modified", "status"}
MARKDOWNLINT = "markdownlint-cli2@0.20.0"
MERMAID = "@mermaid-js/mermaid-cli@11.12.0"


def run(args, **kwargs):
    """Launch quoted arguments, including Windows npm command wrappers."""
    command = shutil.which(str(args[0]))
    if not command:
        raise ValueError(f"required tool {args[0]} is missing; provision it before lint-docs")
    result = subprocess.run([command, *args[1:]], check=False, **kwargs)
    if result.returncode:
        raise ValueError(f"{args[0]} failed (exit {result.returncode})")
    return result


def manifest(root):
    result = run(["git", "ls-files", "-z"], cwd=root, capture_output=True)
    return [os.fsdecode(p) for p in result.stdout.split(b"\0")
            if p and os.fsdecode(p).endswith(".md")]


def scoped(paths):
    return [p for p in paths if p.startswith("docs/") or p in {"README.md", "AGENTS.md"}]


def link_inputs(paths):
    """Check every authored Markdown link; only the vendored tree is excluded."""
    return [p for p in paths if not p.startswith(VENDORED_PREFIX)]


def frontmatter(path, text):
    lines = text.splitlines()
    if not lines or lines[0] != "---":
        raise ValueError(f"{path}: frontmatter requires {', '.join(sorted(REQUIRED))}")
    try:
        end = lines.index("---", 1)
    except ValueError as error:
        raise ValueError(f"{path}: unterminated frontmatter") from error
    fields = {}
    for line in lines[1:end]:
        if not line.strip() or line.startswith("#"):
            continue
        match = re.fullmatch(r"([a-z][a-z -]*):\s*(.*?)\s*", line)
        if not match:
            raise ValueError(f"{path}: malformed top-level scalar frontmatter: {line}")
        key, value = match.groups()
        if key in fields:
            raise ValueError(f"{path}: duplicate frontmatter field {key}")
        if value.startswith(('"', "'")):
            quote = value[0]
            if len(value) < 2 or value[-1] != quote:
                raise ValueError(f"{path}: malformed quoted field {key}")
            value = value[1:-1].strip()
        elif value.startswith(("[", "{", "!", "&", "*", "|", ">")):
            raise ValueError(f"{path}: field {key} must be a scalar")
        value = re.split(r"\s+#", value, maxsplit=1)[0].strip()
        if not value or value.lower() in {"null", "~"} or value.startswith("#"):
            raise ValueError(f"{path}: empty frontmatter field {key}")
        fields[key] = value
    missing = REQUIRED - fields.keys()
    if missing:
        raise ValueError(f"{path}: missing frontmatter fields {', '.join(sorted(missing))}")


def fences(path, text):
    """Honor outer fences, matching character and closing length, and indentation."""
    active = None
    blocks = []
    body = []
    for number, line in enumerate(text.splitlines(), 1):
        match = re.match(r"^ {0,3}(`{3,}|~{3,})(.*)$", line)
        if active:
            char, length, start, mermaid = active
            if match and match[1][0] == char and len(match[1]) >= length and not match[2].strip():
                if mermaid:
                    blocks.append((start, "\n".join(body) + "\n"))
                active, body = None, []
            elif mermaid:
                body.append(line)
        elif match:
            marker, info = match.groups()
            if marker[0] == "`" and "`" in info:
                continue
            active = (marker[0], len(marker), number, info.strip() == "mermaid")
    if active and active[3]:
        raise ValueError(f"{path}:{active[2]}: unclosed Mermaid fence")
    return blocks


def check_index(root, docs):
    index = root / "docs/README.md"
    text = index.read_text(encoding="utf-8")
    # Inline and reference Markdown links, with optional angle-bracket paths.
    targets = re.findall(r"\]\(<?([^\s)>]+)>?(?:\s+[^)]*)?\)", text)
    targets += re.findall(r"^ {0,3}\[[^]]+\]:\s*<?([^\s>]+)>?", text, re.MULTILINE)
    linked = set()
    for target in targets:
        url = urlsplit(target)
        if not url.scheme and not url.netloc:
            linked.add((index.parent / unquote(url.path)).resolve())
    missing = [p for p in docs if (root / p).resolve() not in linked]
    if missing:
        raise ValueError("docs/README.md: missing index entries: " + ", ".join(missing))


def adr_findings(files):
    """Return every ADR identity violation, independent of directory discovery."""
    findings = []
    numbers = {}
    for name, text in sorted(files):
        if name == "README.md":
            continue
        match = re.fullmatch(r"([0-9]{4})-[a-z0-9-]+\.md", name)
        if not match:
            findings.append(f"docs/adr/{name}: ADR file name must be NNNN-<slug>.md")
            continue
        number = match[1]
        numbers.setdefault(number, []).append(name)
        heading = next((line for line in text.splitlines() if line.startswith("# ")), "")
        heading_number = re.match(r"# ([0-9]{4}):", heading)
        if heading_number and heading_number[1] != number:
            findings.append(f"docs/adr/{name}: heading number {heading_number[1]} "
                            f"does not match file number {number}")
    for number, names in sorted(numbers.items()):
        if len(names) > 1:
            findings.append(f"docs/adr: ADR number {number} used by {', '.join(names)}")
    return findings


def compile_mermaid(blocks, temp):
    for index, (path, line, body) in enumerate(blocks):
        source = temp / f"diagram-{index}.mmd"
        output = temp / f"diagram-{index}.svg"
        source.write_text(body, encoding="utf-8")
        print(f"Mermaid: {path}:{line}", flush=True)
        try:
            run(["npx", "--yes", MERMAID, "-i", str(source), "-o", str(output)])
        except ValueError as error:
            raise ValueError(f"{path}:{line}: Mermaid compile failed; ensure the pinned CLI "
                             f"and its supported headless browser are provisioned: {error}") from error
        if not output.is_file() or output.stat().st_size == 0:
            raise ValueError(f"{path}:{line}: Mermaid produced no SVG")
    print(f"Mermaid: compiled {len(blocks)} blocks", flush=True)


def check(root, online=False):
    print(f"lint-docs: vendored exclusion VENDORED_PREFIX={VENDORED_PREFIX}", flush=True)
    paths = manifest(root)
    selected = scoped(paths)
    docs = [p for p in selected if p.startswith("docs/")]
    blocks = []
    for path in paths:
        text = (root / path).read_text(encoding="utf-8")
        if path in docs:
            frontmatter(path, text)
        blocks.extend((path, line, body) for line, body in fences(path, text))
    print(f"Frontmatter: {len(docs)} documents", flush=True)
    check_index(root, docs)
    print("Index: all tracked docs listed", flush=True)
    adr_directory = root / "docs/adr"
    try:
        adr_files = list(adr_directory.iterdir())
    except FileNotFoundError:
        adr_files = []
    findings = adr_findings([(path.name, path.read_text(encoding="utf-8"))
                             for path in adr_files if path.is_file()])
    if findings:
        raise ValueError("\n".join(findings))
    print("Markdown style", flush=True)
    run(["npx", "--yes", MARKDOWNLINT, *selected], cwd=root)
    print("Links: " + ("online" if online else "offline"), flush=True)
    args = ["lychee", "--config", "lychee.toml"]
    if not online:
        args.append("--offline")
    run([*args, *link_inputs(paths)], cwd=root)
    with tempfile.TemporaryDirectory(prefix="lint-docs-") as directory:
        compile_mermaid(blocks, Path(directory))
    print("lint-docs: passed", flush=True)


def main():
    if sys.argv[1:] not in ([], ["--online"]):
        print("usage: lint_docs.py [--online]", file=sys.stderr)
        return 2
    os.environ["npm_config_offline"] = "true"
    # Raising permits TemporaryDirectory cleanup on termination as on Ctrl-C.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    try:
        check(Path.cwd(), online=bool(sys.argv[1:]))
    except (OSError, ValueError) as error:
        print(f"lint-docs: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
