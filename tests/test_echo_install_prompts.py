"""Exercise release-seed provenance through the manual installer and fake adb."""
import hashlib
import os

from test_echo_install import DEVICE, ROOT, TIME_ZONE_HINT, Sandbox, owner, require

NAMES = ("runner", "voice")
FAILURE = "echo-install: device operation failed\n"
HISTORICAL_PROMPTS = ROOT / "tests/fixtures/echo-install-prompts"
# 218736fe changed runner.md only. Both releases shipped the same voice blob.
HISTORICAL_RELEASES = (
    ("3313a7da-runner.md", "3313a7da-voice.md"),
    ("e707712d-runner.md", "e707712d-voice.md"),
    ("218736fe-runner.md", "e707712d-voice.md"),
)


def prompt_path(box, name):
    return box.device / "home/prompts" / (name + ".md")


def seed_record(box, name):
    return box.device / "private/prompt-seeds" / (name + ".sha256")


def stored_seed(applied, pending=None):
    applied_hash = "-" if applied is None else hashlib.sha256(applied).hexdigest()
    pending_hash = "-" if pending is None else hashlib.sha256(pending).hexdigest()
    return f"version 1\napplied {applied_hash}\npending {pending_hash}\n".encode()


def ship_prompt(box, name, data):
    example = box.root / "examples/prompts" / (name + ".md")
    example.parent.mkdir(parents=True, exist_ok=True)
    example.write_bytes(data)


def check_output(result, **dispositions):
    expected = "".join(f"echo-install: prompt {name}.md {dispositions[name]}\n"
                       for name in sorted(dispositions))
    require(result.stdout == expected + "echo-install: installed\n" and result.stderr == TIME_ZONE_HINT + "\n",
            "prompt disposition differs")


def check_seed(box, name, data):
    prompt, record = prompt_path(box, name), seed_record(box, name)
    require(prompt.read_bytes() == data, "prompt bytes differ")
    require(record.read_bytes() == stored_seed(data), "provenance bytes differ")
    for path, mode in ((prompt, 0o600), (record, 0o600), (record.parent, 0o700)):
        require(path.stat().st_mode & 0o777 == mode, "prompt state mode differs")
        require(owner(box.root, path) == "9999:9999", "prompt state owner differs")


def identity(path):
    info = path.stat()
    return info.st_ino, info.st_mtime_ns, path.read_bytes()


def check_retry_cleanup(box):
    require(not (box.device / "private/.install-stage").exists(), "private stage leaked")
    require(not (box.device.parent / "echo-bridge.lock").exists(), "install lock leaked")


def historical_cases():
    for release in HISTORICAL_RELEASES:
        with Sandbox() as box:
            for name, fixture in zip(NAMES, release, strict=True):
                previous = (HISTORICAL_PROMPTS / fixture).read_bytes()
                installed = prompt_path(box, name)
                installed.parent.mkdir(parents=True, exist_ok=True)
                installed.write_bytes(previous)
                ship_prompt(box, name, previous + b"New release.\r\n")
            check_output(box.install(), runner="refreshed", voice="refreshed")
            for name in NAMES:
                example = box.root / "examples/prompts" / (name + ".md")
                check_seed(box, name, example.read_bytes())


def current_and_edited_cases():
    with Sandbox() as box:
        original = {name: f"Release one {name}\r\n".encode() for name in NAMES}
        for name, data in original.items():
            ship_prompt(box, name, data)
        check_output(box.install(), runner="seeded", voice="seeded")
        tracked = [path for name in NAMES for path in (prompt_path(box, name), seed_record(box, name))]
        before = [identity(path) for path in tracked]
        check_output(box.install(), runner="current", voice="current")
        require([identity(path) for path in tracked] == before, "current prompt state was rewritten")
        for name, data in original.items():
            check_seed(box, name, data)

        # Even historical release bytes for a different prompt are owner edits here.
        edited = (HISTORICAL_PROMPTS / "3313a7da-runner.md").read_bytes()
        voice = prompt_path(box, "voice")
        voice.write_bytes(edited)
        before = identity(voice), identity(seed_record(box, "voice"))
        updated = b"Release two runner\n"
        ship_prompt(box, "runner", updated)
        ship_prompt(box, "voice", b"Release two voice\n")
        check_output(box.install(force=True), runner="refreshed", voice="kept")
        require((identity(voice), identity(seed_record(box, "voice"))) == before,
                "owner prompt or its provenance was rewritten")
        check_seed(box, "runner", updated)

    with Sandbox() as box:
        data = (ROOT / "examples/prompts/voice.md").read_bytes()
        ship_prompt(box, "voice", data)
        voice = prompt_path(box, "voice")
        voice.parent.mkdir(parents=True)
        voice.write_bytes(data)
        before = identity(voice)
        check_output(box.install(), voice="current")
        require(identity(voice) == before, "legacy current prompt was rewritten")
        check_seed(box, "voice", data)

        # Matching a digest in a truncated record cannot authorize replacement.
        voice.write_bytes(b"Owner's own words")
        record = seed_record(box, "voice")
        record.write_bytes(stored_seed(voice.read_bytes())[:-1])
        before = identity(voice), identity(record)
        check_output(box.install(), voice="kept")
        require((identity(voice), identity(record)) == before, "malformed record authorized an overwrite")


def interrupted_cases():
    previous, candidate, later = (b"First runner release\n", b"Second runner release\r\n", b"Third runner release")
    for after_rename in (False, True):
        for retry in (candidate, later):
            with Sandbox() as box:
                ship_prompt(box, "runner", previous)
                check_output(box.install(), runner="seeded")
                ship_prompt(box, "runner", candidate)
                if after_rename:
                    fault = f"'applied {hashlib.sha256(candidate).hexdigest()}' 'pending -'"
                else:
                    fault = (f"mv -f '{DEVICE}/private/.install-stage' "
                             f"'{DEVICE}/home/prompts/runner.md'")
                (box.root / "fault").write_text(fault)
                failed = box.install(code=1)
                require(failed.stderr == FAILURE and not failed.stdout, "publication failure output differs")
                prompt = prompt_path(box, "runner")
                require(prompt.read_bytes() == (candidate if after_rename else previous),
                        "interrupted publication produced unexpected prompt bytes")
                require(seed_record(box, "runner").read_bytes() == stored_seed(previous, candidate),
                        "interrupted publication did not preserve both digests")
                check_retry_cleanup(box)
                ship_prompt(box, "runner", retry)
                disposition = "current" if after_rename and retry == candidate else "refreshed"
                check_output(box.install(), runner=disposition)
                check_seed(box, "runner", retry)
                check_retry_cleanup(box)


def refusal_cases():
    for surface, obstruction in ((prompt_path, "symlink"), (prompt_path, "fifo"),
                                 (seed_record, "symlink"), (seed_record, "directory")):
        with Sandbox() as box:
            ship_prompt(box, "runner", b"Release runner")
            blocked = surface(box, "runner")
            blocked.parent.mkdir(parents=True)
            outside = box.root / "outside"
            outside.write_bytes(b"outside bytes")
            if obstruction == "symlink":
                blocked.symlink_to(outside)
            elif obstruction == "fifo":
                os.mkfifo(blocked)
            else:
                blocked.mkdir(mode=0o755)
                (blocked / "unexpected").write_bytes(b"unrelated bytes")
            result = box.install(code=1)
            require(result.stderr == FAILURE and not result.stdout, "unsafe target output differs")
            require(outside.read_bytes() == b"outside bytes", "unsafe target was followed")
            if obstruction == "directory":
                require(blocked.stat().st_mode & 0o777 == 0o755, "unexpected directory was modified")
            check_retry_cleanup(box)

    with Sandbox() as box:
        ship_prompt(box, "runner", b"Release runner")
        lock = box.device.parent / "echo-bridge.lock"
        lock.mkdir()
        before = lock.stat().st_ino
        result = box.install(code=1)
        require(result.stderr == FAILURE and not result.stdout, "busy lock output differs")
        require(lock.stat().st_ino == before and not box.device.exists(), "install took over another lock")


def prompt_seed_cases():
    historical_cases()
    current_and_edited_cases()
    interrupted_cases()
    with Sandbox() as box:
        result = box.install()
        require(result.stdout == "echo-install: prompt defaults absent; skipped\necho-install: installed\n"
                and result.stderr == TIME_ZONE_HINT + "\n", "absent defaults output differs")
        require(not list((box.device / "home/prompts").iterdir())
                and not seed_record(box, "runner").parent.exists(), "absent defaults created seed state")
    refusal_cases()
    print("echo-install prompt-seed cases: P1 P2 P3 P4 P5 P6 P7 PASS", flush=True)
