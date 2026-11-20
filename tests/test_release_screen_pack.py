"""T3 pinned screen-pack installs through the native CLI and delivered shell."""
import io
import json
from pathlib import Path
import sys
import tarfile
import tomllib

from test_echo_install import ROOT, require, set_owner
from test_release_delivery import DeliveryBox, assert_installed_pack, delivered_journey, delivery_calls, local_endpoint
from test_release_kit import PACK, sha


SCREEN = ('[screen]\nlisten = "127.0.0.1:16900"\n'
          'pairing_token_file = "/data/local/echo-bridge/private/screen-pairing.txt"\n')


def write_archive(path, members):
    with tarfile.open(path, "w:gz", format=tarfile.USTAR_FORMAT) as archive:
        for name, data in members.items():
            header = tarfile.TarInfo(name)
            header.size = len(data)
            archive.addfile(header, io.BytesIO(data))


def release_fixture(box, name, *, unsafe=False, tampered=False, default_status=200, row=True):
    release = box.release
    identity, asset = f"{name}@{release}", f"{name}-{release}.screenpack.tar.gz"
    default, template = (f"{kind}-{release}.screenpack.tar.gz" for kind in ("default", "template"))
    box.assets.mkdir()
    members = {"pack.json": json.dumps({"format": 1, "name": name, "version": release,
                                       "character": {"rest": "rest.png"}}).encode(), "rest.png": b"fixture rest"}
    write_archive(box.assets / asset, {**members, "../escape.png": b"unsafe"} if unsafe else members)
    digest = sha((box.assets / asset).read_bytes())
    if tampered:
        (box.assets / asset).write_bytes(b"changed after authentication")
    url = f"https://github.com/portpowered/portos-echo/releases/download/v{release}/{asset}"
    sample = SCREEN + f'packs = [{{ url = "{url}", sha256 = "{digest}" }},' \
             ' { url = "https://example.invalid/extra", sha256 = "' + "a" * 64 + '" }]\n'
    files = {key: b"fixture runtime" for key in PACK.REQUIRED | PACK.SETUP_SCRIPTS}
    files.update({"examples/bridge.echo.toml": sample.encode(), "examples/prompts/voice.md": b"voice",
                  "examples/prompts/runner.md": b"runner"})
    for key in ("echo-install.sh", "echo-install-release.sh", "echo-install-setup.sh"):
        files[f"scripts/{key}"] = (ROOT / "scripts" / key).read_bytes()
    manifest = {"format": 1, "version": release, "files": {key: sha(data) for key, data in files.items()}}
    write_archive(box.archive, {**files, "MANIFEST": json.dumps(manifest).encode()})
    checksums = {box.archive.name: sha(box.archive.read_bytes())}
    if row:
        checksums[asset] = digest
    if name == "default":
        legacy = {**members, "pack.json": members["pack.json"].replace(b'"default"', b'"template"')}
        write_archive(box.assets / template, legacy)
        checksums[template] = sha((box.assets / template).read_bytes())
    (box.assets / "SHA256SUMS").write_text("".join(f"{value}  {key}\n" for key, value in checksums.items()))
    if default_status != 200:
        box.responses[default] = default_status
    return manifest, identity, asset, digest, members


def owned_file(box, path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    for directory in path.parents:
        if directory == box.device:
            break
        directory.chmod(0o700)
        set_owner(box.root, directory, "9999:9999")
    path.write_bytes(data)
    path.chmod(0o600)
    set_owner(box.root, path, "9999:9999")


def case_install(box, name, variant, **options):
    _, identity, asset, digest, members = release_fixture(box, name, **options)
    home = box.device / "home"
    root = home / "screens" / identity
    retained = {home / "settings.toml": b'screen_pack = ""\n',
                home / "screens/other@1.0.0/keep.png": b"other installed data",
                box.device / "private/bridge.toml": b"# operator config\n" + SCREEN.encode()}
    if variant in ("retained", "same", "conflict"):
        for path, data in retained.items():
            owned_file(box, path, data)
    if variant in ("same", "conflict"):
        for path, data in members.items():
            owned_file(box, root / path, data if variant == "same" else b"operator bytes")
        inode = root.stat().st_ino
    success = variant in ("fresh", "retained", "same")
    if success:
        (box.root / "expected-pack").write_text(json.dumps([identity, digest, {k: v.hex() for k, v in members.items()}]))
    with local_endpoint(box) as screen, local_endpoint(box) as health, local_endpoint(box, True) as release_port:
        (box.root / "ports.json").write_text(json.dumps({"tcp:16900": screen, "tcp:16901": health}))
        args = ("install", "--version", box.release, "--release-base", f"http://127.0.0.1:{release_port}")
        if variant in ("same", "conflict"):
            result = box.run_setup(*args, json_mode=False)
            require(root.stat().st_ino == inode, "reinstall replaced the existing identity")
            if success:
                require(result.returncode == 0 and result.stderr == "" and result.stdout.splitlines()[-1] ==
                        f"Installed screen pack {asset}.", "human success result differs")
            else:
                require(result.returncode == 1 and not result.stdout and result.stderr ==
                        "echo-setup: install_failed. Check device connectivity and prerequisites, then run install again.\n"
                        and all((root / path).read_bytes() == b"operator bytes" for path in members),
                        "destination conflict result or retained bytes differ")
        elif success:
            event = box.invoke(*args)
            require(event["version"] == box.release and event["screen_pack_asset"] == asset, "JSON asset differs")
        else:
            result = box.run_setup(*args)
            assert_refusal(result, "unsafe_archive_or_missing_device_tools")
            require(not root.exists() and not (box.device / "release.json").exists(), "unsafe pack mutated the release")
    require(not any((box.root / "remote/tmp").iterdir()), "pack install retained its stage")
    if success:
        assert_installed_pack(box.root, identity, digest, members)
        require(json.loads((box.root / "forwards.json").read_text()) == {}, "pack install retained forwards")
    else:
        require(not any("--device install --bundle" in call[-1] or "--device start" in call[-1]
                        for call in delivery_calls(box.root)), "refused pack reached bundled install or start")
    if variant == "fresh":
        config = tomllib.loads((box.device / "private/bridge.toml").read_text())
        expected_config = tomllib.loads(SCREEN)
        expected_config["screen"]["packs"] = [{"url": "https://example.invalid/extra", "sha256": "a" * 64}]
        require(config == expected_config,
                "config seed did not remove only the authenticated release pin")
    elif variant in ("retained", "same", "conflict"):
        require(all(path.read_bytes() == data for path, data in retained.items()), "operator config or home bytes changed")
    expected = [(f"/v{box.release}/default-{box.release}.screenpack.tar.gz", 404)] if name == "template" else []
    assert_requests(box, [*expected, (f"/v{box.release}/{asset}", 200)])


def assert_requests(box, packs):
    expected = [(f"/v{box.release}/{leaf}", 200) for leaf in ("SHA256SUMS", box.archive.name)] + packs
    require(box.requests == expected, f"release request order differs: expected {expected}, got {box.requests}")


def assert_refusal(result, error):
    event = json.loads(result.stdout)
    require(result.returncode == 1 and not result.stderr and event["ok"] is False and event["error"] == error
            and "screen_pack_asset" not in event, "incorrect pack refusal: " + result.stdout)


def pack_refusals(binary):
    cases = (
        ("0.2.0", "default", {"tampered": True}, [200], "checksum_mismatch"),
        ("0.2.0", "template", {"default_status": 503}, [503], "download_failed"),
        ("0.2.0", "template", {"tampered": True}, [404, 200], "checksum_mismatch"),
        ("0.3.0", "template", {}, [404], "download_failed"),
        ("0.2.0", "template", {"row": False}, [404], "invalid_checksum"),
        ("0.2.0", "default", {"row": False}, [200], "invalid_checksum"),
        ("0.2.0", "template", {}, [404, 404], "download_failed"),
    )
    for release, name, options, statuses, error in cases:
        with Fixture(binary, release) as box:
            _, _, asset, _, _ = release_fixture(box, name, **options)
            if statuses == [404, 404]:
                (box.assets / asset).unlink()
            with local_endpoint(box, True) as port:
                result = box.run_setup("install", "--version", release, "--release-base", f"http://127.0.0.1:{port}")
            assert_refusal(result, error)
            paths = [f"/v{release}/{kind}-{release}.screenpack.tar.gz" for kind in ("default", "template")]
            assert_requests(box, list(zip(paths, statuses)))
            require(delivery_calls(box.root) == [] and not box.device.exists(), "pack refusal reached the device")


class Fixture(DeliveryBox):
    def __init__(self, binary, release="0.3.0"):
        # Use an isolated asset directory while keeping the runtime basename canonical.
        super().__init__(binary, None, None, release)
        self.assets = self.root / "assets"
        self.archive = self.assets / f"echo-{release}-armv7.tar.gz"


def main(binary, case):
    if case == "refusals":
        pack_refusals(binary)
    elif case == "offline":
        with Fixture(binary) as box:
            manifest, *_ = release_fixture(box, "default")
            delivered_journey(binary, box.assets, box.archive, manifest, pin_version=True, check_maintenance=False)
    else:
        legacy = case == "retained"
        with Fixture(binary, "0.2.0" if legacy else "0.3.0") as box:
            case_install(box, "template" if legacy else "default", case, unsafe=case == "unsafe")
    print(f"release screen pack: {case}=PASS", flush=True)


if __name__ == "__main__":
    main(Path(sys.argv[1]), sys.argv[2])
