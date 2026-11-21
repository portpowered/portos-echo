"""Release verifier witnesses for optional assets and sanitized diagnostics."""
import hashlib
import io
from itertools import chain
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
from types import SimpleNamespace
from unittest.mock import patch

from test_echo_install import require

ROOT = Path(__file__).resolve().parents[1]


def companion_stylesheet_faults(data, sums, row, name):
    for remove_stylesheet in (True, False):
        archive = io.BytesIO()
        with tarfile.open(fileobj=io.BytesIO(data)) as source, \
                tarfile.open(fileobj=archive, mode='w:gz', compresslevel=1) as target:
            for member in source:
                if remove_stylesheet and member.name == 'companion-clock.css':
                    continue
                contents = source.extractfile(member).read()
                if not remove_stylesheet and member.name == 'pack.json':
                    manifest = json.loads(contents)
                    del manifest['layout']['css']
                    contents = json.dumps(manifest).encode()
                    member.size = len(contents)
                target.addfile(member, io.BytesIO(contents))
        content = archive.getvalue()
        substitute = f'{hashlib.sha256(content).hexdigest()}  {name}\n'
        yield content, sums.replace(row, substitute), 'invalid companion stylesheet'


def companion_cases(assets, pack, witness):
    name = 'companion-0.2.2.screenpack.tar.gz'
    archive = next(assets.glob('echo-*-armv7.tar.gz')).name
    sums = (assets / 'SHA256SUMS').read_text()
    data = (assets / name).read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    row = f'{digest}  {name}\n'
    require(sums.count(row) == 1, 'expected one exact Companion digest row')
    with tarfile.open(assets / name) as source:
        manifest = json.load(source.extractfile('pack.json'))
    require((manifest['format'], manifest['name'], manifest['version']) == (1, 'companion', '0.2.2'),
            'Companion identity differs')
    default_pack = next(assets.glob('default-*.screenpack.tar.gz')).read_bytes()
    substitute = f'{hashlib.sha256(default_pack).hexdigest()}  {name}\n'
    faults = (
        (None, sums, 'invalid companion asset membership'),
        (data, sums.replace(row, ''), 'invalid companion asset membership'),
        (data + b'changed', sums, 'asset checksum mismatch'),
        (data, sums + row, 'invalid checksums'),
        (default_pack, sums.replace(row, substitute), 'invalid companion identity'),
        (None, sums.replace(row, ''), None),
    )
    with tempfile.TemporaryDirectory(prefix='companion-faults-') as temporary:
        cases = chain(faults, companion_stylesheet_faults(data, sums, row, name))
        for index, (content, checksums, diagnostic) in enumerate(cases):
            variant = Path(temporary) / str(index)
            shutil.copytree(assets, variant)
            (variant / 'SHA256SUMS').write_text(checksums)
            if content is None:
                (variant / name).unlink()
            else:
                (variant / name).write_bytes(content)
            verify = [sys.executable, str(ROOT / 'scripts/package-dist.py'), 'verify',
                      str(variant / archive), '--checksums', str(variant / 'SHA256SUMS')]
            result = subprocess.run(verify, capture_output=True, text=True, timeout=15)
            if diagnostic is None:
                require(result.returncode == 0 and not result.stderr, 'six-asset verification failed')
                current = subprocess.run(
                    [sys.executable, str(ROOT / 'tests/test_release_kit.py'), '--assets', str(variant)],
                    capture_output=True, text=True, timeout=15)
                require(current.returncode == 1 and 'current dist requires Companion' in current.stderr,
                        'current release accepted a legacy dist')
                result = subprocess.run(verify + ['--require-companion'], capture_output=True, text=True, timeout=15)
                diagnostic = 'invalid companion asset membership'
                pre_setup_cases(variant, variant / archive, verify)
            require(result.returncode == 1 and result.stdout == ''
                    and result.stderr == f'release-kit: {diagnostic}\n', 'Companion refusal changed')
    print('RELEASE-CSS missing_member=REFUSED missing_manifest_reference=REFUSED matching_digests=2')
    try:
        pack.build(SimpleNamespace(out=assets))
    except pack.BundleError as error:
        require(str(error).startswith('output directory must not exist'), 'occupied dist refusal changed')
    else:
        raise AssertionError('occupied dist accepted')
    require((assets / 'SHA256SUMS').read_text() == sums and (assets / name).read_bytes() == data,
            'prior dist changed after refusal')
    if witness is not None:
        witness.mkdir()
        shutil.copyfile(assets / name, witness / name)
    print(f'RELEASE-ASSET name={name} sha256={digest} assets=7 matching_rows=1 legacy=3+6 refusal=PASS prior=preserved')


def pre_setup_cases(assets, archive, verify):
    sums = assets / 'SHA256SUMS'
    rows = sums.read_text().splitlines(keepends=True)
    for row in rows:
        name = row.split()[1]
        if name.startswith('echo-setup-') or name == 'install.ps1':
            (assets / name).unlink()
    base = {archive.name, 'install.sh', next(assets.glob('default-*.screenpack.tar.gz')).name}
    rows = {row.split()[1]: row for row in rows if row.split()[1] in base}
    sums.write_text(''.join(rows.values()))
    result = subprocess.run(verify, capture_output=True, text=True, timeout=15)
    require((result.returncode, result.stdout, result.stderr) ==
            (1, '', 'release-kit: asset checksum mismatch\n'), 'setup runtime accepted missing setup assets')

    with tarfile.open(archive) as source:
        payload = {member.name: (member, source.extractfile(member).read()) for member in source.getmembers()}
    manifest_entry, manifest_data = payload.pop('MANIFEST')
    manifest = json.loads(manifest_data)
    for name in ('scripts/echo-install-release.sh', 'scripts/echo-install-setup.sh'):
        del payload[name], manifest['files'][name]
    with tarfile.open(archive, 'w:gz') as target:
        for member, data in payload.values():
            target.addfile(member, io.BytesIO(data))
        data = json.dumps(manifest).encode()
        manifest_entry.size = len(data)
        target.addfile(manifest_entry, io.BytesIO(data))
    rows[archive.name] = f'{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n'
    sums.write_text(''.join(rows.values()))
    result = subprocess.run(verify, capture_output=True, text=True, timeout=15)
    require(result.returncode == 0 and not result.stderr, 'three-asset verification failed')


def path_diagnostic_cases(pack, assemble_fixture):
    paths = (b'/home/example/.cargo/registry/src/index/public-1.0/src/lib.rs',
             b'/Users/example/.rustup/toolchains/stable/lib/source.rs',
             b'C:\\Users\\example\\private\\file')
    counts = pack.private_path_counts(b'\x00' + b'\x00'.join(paths) + b'\x00' + paths[0])
    require(counts == {"registry": 2, "toolchain": 1, "checkout": 0, "other": 1,
                       "current_home": 0, "current_cargo": 0, "current_rustup": 0, "external": 4},
            "sanitized path category counts differ")
    require(all(count == 0 for count in pack.private_path_counts(b'/data/local/echo/home/prompts').values()),
            "device home classified as a host path")
    for boundary in (b'\v', b'\f', b'='):
        require(pack.private_path_counts(boundary + paths[0])["registry"] == 1,
                "path delimiter changed classification")
    with patch.object(pack.Path, "home", return_value=Path("/home/example")), \
            patch.object(pack, "ROOT", Path("/home/example/source")):
        flags = b'-ffile-prefix-map=/home/example=/build/user -ffile-prefix-map=/home/example/source=/build/echo\x00'
        mapped = pack.private_path_counts(flags * 2)
        require(mapped["checkout"] == 2 and mapped["other"] == 2 and mapped["current_home"] == 4
                and mapped["external"] == 0, "remap arguments misclassified as external paths")
    with tempfile.TemporaryDirectory() as temporary:
        assets = Path(temporary) / "assets"
        assets.mkdir()
        fixture = assets.parent / "public"
        fixture.write_bytes(b"synthetic public input")
        try:
            assemble_fixture(assets, fixture, fixture, (fixture, fixture), private_binary=b'\x00'.join(paths))
            raise AssertionError("private binary accepted")
        except pack.BundleError:
            pass
        result = subprocess.run([sys.executable, str(ROOT / "scripts/package-dist.py"), "verify",
                                 str(assets / "echo-0.1.0-armv7.tar.gz"), "--checksums",
                                 str(assets / "SHA256SUMS")], capture_output=True, text=True)
        require(result.returncode == 1 and result.stdout == "", "private binary CLI status differs")
        expected = dict(counts, registry=1, external=3)
        require(result.stderr == "release-kit: rejected path categories " + json.dumps(expected, sort_keys=True)
                + "\nrelease-kit: private path or key in bin/codex-app-server\n", "diagnostic leaked or counts differ")
    print("U-PACK sanitized path diagnostics: PASS")
