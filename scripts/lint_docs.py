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


def check_receiver_uses(body, alias, accesses, name):
    """Reject passing, aliasing or capturing Fields outside the known grammar."""
    allowed = {call.start() for call in accesses}
    for binding in re.finditer(r'if let Some\(mut (\w+)\) = \w+\.section\("\w+"\)\?', body):
        if binding[1] == alias:
            allowed.add(binding.start(1))
    # Preserve offsets while ignoring string contents and member names such as
    # result.voice. Every other receiver occurrence must be a checked access.
    code = re.sub(r'"(?:\\.|[^"\\])*"', lambda match: ' ' * len(match[0]), body)
    for use in re.finditer(rf'\b{alias}\b', code):
        if code[:use.start()].rstrip().endswith('.'):
            continue
        if use.start() not in allowed:
            raise ValueError(f"config coverage: unsupported Fields receiver use ({alias}) in {name}; "
                             "helper calls, aliases and captures require decoder reader support")


def decoder_keys(source, name):
    """Read the deliberately narrow Fields grammar; reject unsupported accesses.

    This is a static lint, not a Rust evaluator. A decoder refactor must teach
    this reader its new grammar before it can certify documentation coverage.
    """
    source = source.split("#[cfg(test)]", 1)[0]
    source = re.sub(r"//[^\n]*", "", source)
    starts = list(re.finditer(r'let mut (\w+) = Fields::new\(value,\s*'
                             r'(?:"([\w.]+)"|&format!\("([\w.]+)\[\{index\}\]"\))\)\?;', source))
    if len(starts) != source.count("Fields::new("):
        raise ValueError(f"config coverage: unsupported Fields constructor in {name}")
    if "fn decode(" in source and not starts:
        raise ValueError(f"config coverage: missing Fields decoder in {name}")
    keys = set()
    for start in starts:
        receiver, literal, array = start.groups()
        prefix = "" if literal == "config" else (literal or array + "[]")
        end = re.search(rf'\b{receiver}\.finish\(\)\?', source[start.end():])
        if not end:
            raise ValueError(f"config coverage: missing finish in {name}")
        body = source[start.end():start.end() + end.start()]
        aliases = {receiver: prefix}
        for alias, parent, key in re.findall(
                r'if let Some\(mut (\w+)\) = (\w+)\.section\("(\w+)"\)\?', body):
            if parent not in aliases:
                raise ValueError(f"config coverage: unsupported section parent in {name}")
            aliases[alias] = ".".join(filter(None, [aliases[parent], key]))
        if len(re.findall(r'\.section\s*\(', body)) != len(aliases) - 1:
            raise ValueError(f"config coverage: unsupported section binding in {name}")
        for alias, path in aliases.items():
            # Consume every access, not just known string calls: a dynamic key
            # or a new Fields operation must fail rather than disappear.
            access = rf'\b{alias}\s*\.\s*(?:table\s*\.\s*)?(\w+)\s*\('
            if re.search(rf'\b{alias}\s*\.\s*\w+\s*::', body):
                raise ValueError(f"config coverage: unsupported generic access in {name}")
            accesses = list(re.finditer(access, body))
            for call in accesses:
                method = call[1]
                if method == "finish":
                    continue
                argument = re.match(r'"(\w+)"', body[call.end():])
                compatibility = {"reject", "discard", "compatibility_value"}
                if method not in {"required", "optional", "node", "remove", "section"} | compatibility or not argument:
                    raise ValueError(f"config coverage: unsupported key access in {name}")
                if method not in compatibility:
                    key = ".".join(filter(None, [path, argument[1]]))
                    keys.add(key)
            check_receiver_uses(body, alias, accesses, name)
        if prefix:
            keys.add(prefix)
    return keys


def accepted_config_keys(root):
    directory = root / "crates/echo-config/src"
    lib = (directory / "lib.rs").read_text(encoding="utf-8").split("#[cfg(test)]", 1)[0]
    modules = set(re.findall(r'^mod (config_\w+);$', lib, re.MULTILINE))
    if not {"config_decode", "config_settings"} <= modules:
        raise ValueError("config coverage: required decoder modules missing")
    keys = set()
    for module in sorted(modules):
        path = directory / (module + ".rs")
        try:
            source = path.read_text(encoding="utf-8")
        except FileNotFoundError:
            path = directory / module / "mod.rs"
            source = path.read_text(encoding="utf-8")
        name = path.relative_to(directory).as_posix()
        extracted = decoder_keys(source, name)
        if module != "config_validate" and not extracted:
            raise ValueError(f"config coverage: empty or unsupported decoder in {name}")
        keys.update(extracted)
    if not keys:
        raise ValueError("config coverage: empty accepted key inventory")
    return keys


def config_coverage(root):
    keys = accepted_config_keys(root)
    text = (root / "docs/configuration.md").read_text(encoding="utf-8")
    # Only the first cell of an actual reference row counts. Prose, fenced
    # examples and HTML comments cannot accidentally satisfy coverage.
    text = re.sub(r'<!--.*?-->', '', text, flags=re.DOTALL)
    rows, active = set(), None
    for line in text.splitlines():
        fence = re.match(r'^ {0,3}(`{3,}|~{3,})(.*)$', line)
        if fence:
            if active is None:
                active = fence[1]
            elif fence[1][0] == active[0] and len(fence[1]) >= len(active) and not fence[2].strip():
                active = None
            continue
        if not active:
            row = re.match(r'^\| `([\w.]+(?:\[\])?(?:\.[\w]+)*)` \|', line)
            if row:
                rows.add(row[1])
    missing = keys - rows
    if missing:
        raise ValueError("docs/configuration.md: undocumented config keys: " + ", ".join(sorted(missing)))
    print(f"Config coverage: {len(keys)} accepted paths documented", flush=True)


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
    config_coverage(root)
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
