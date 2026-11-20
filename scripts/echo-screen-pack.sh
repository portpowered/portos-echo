#!/bin/sh
# Linux author helper. Hardware use belongs to the coordinator; tests fake adb.
set -eu
exec python3 - "$@" <<'PY'
import json
import math
import os
from pathlib import Path
import re
import signal
import stat
import subprocess
import sys
import tempfile
import uuid


class Refused(Exception):
    pass


def check(condition, reason='invalid pack.json'):
    if not condition:
        raise Refused(reason)


def fields(value, required, optional=()):
    check(type(value) is dict and set(required) <= value.keys()
          and value.keys() <= set(required) | set(optional))


def number(value, low, high):
    check(type(value) in (int, float) and math.isfinite(value) and low <= value <= high)


def color(value):
    check(type(value) is str and re.fullmatch(r'#[0-9a-fA-F]{6}', value))


def relative(value):
    check(type(value) is str and re.fullmatch(r'[A-Za-z0-9_./-]+', value)
          and all(part not in ('', '.', '..') for part in value.split('/')),
          'pack paths must be normalized and relative')


def reference(files, value, extensions):
    relative(value)
    check(value in files and value.rsplit('.', 1)[-1] in extensions,
          'missing pack file or wrong file type')


def read_files(root):
    check(not root.is_symlink() and root.is_dir(), 'pack must be a regular directory')
    files, pending, total, entries = {}, [''], 0, 0
    root_fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        return directory_files(root_fd, files, pending, total, entries)
    finally:
        os.close(root_fd)


def directory_files(root_fd, files, pending, total, entries):
    while pending:
        directory = pending.pop()
        fd = os.dup(root_fd)
        try:
            for part in directory.split('/') if directory else ():
                child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
                os.close(fd)
                fd = child
            with os.scandir(fd) as contents:
                names = []
                for entry in contents:
                    check(entries + len(names) < 256, 'too many pack directory entries')
                    names.append(entry.name)
            for leaf in names:
                entries += 1
                check(entries <= 256, 'too many pack directory entries')
                name = directory + '/' + leaf if directory else leaf
                relative(name)
                meta = os.stat(leaf, dir_fd=fd, follow_symlinks=False)
                check(not stat.S_ISLNK(meta.st_mode), 'pack links are forbidden')
                if stat.S_ISDIR(meta.st_mode):
                    pending.append(name)
                    continue
                check(stat.S_ISREG(meta.st_mode) and meta.st_nlink == 1,
                      'pack links and special files are forbidden')
                ext = name.rsplit('.', 1)[-1]
                check(ext in ('json', 'css', 'png', 'webp', 'webm', 'mp4'), 'unsupported pack file type')
                check(len(files) < 64, 'pack exceeds 64 files')
                cap = min(64 * 1024**2 - total, 8 * 1024**2 if ext in ('webm', 'mp4') else 64 * 1024**2)
                check(meta.st_size <= cap, 'pack size limit exceeded')
                files[name] = read_regular(fd, leaf, meta, cap)
                total += len(files[name])
        finally:
            os.close(fd)
    return files


def read_regular(directory_fd, leaf, meta, cap):
    fd = os.open(leaf, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory_fd)
    with os.fdopen(fd, 'rb') as source:
        opened = os.fstat(source.fileno())
        check(stat.S_ISREG(opened.st_mode) and opened.st_nlink == 1
              and (opened.st_dev, opened.st_ino) == (meta.st_dev, meta.st_ino),
              'pack changed while reading')
        data = source.read(cap + 1)
        check(len(data) <= cap, 'pack size limit exceeded')
    return data


def point(value):
    fields(value, ('x', 'y'))
    number(value['x'], 0, 100)
    number(value['y'], 0, 100)


def layout(value, files):
    fields(value, ('safe_area', 'slots', 'stage'), ('css',))
    area = value['safe_area']
    fields(area, ('x', 'y', 'width', 'height'))
    for coordinate in area.values():
        number(coordinate, 0, 100)
    check(area['width'] > 0 and area['height'] > 0
          and area['x'] + area['width'] <= 100 and area['y'] + area['height'] <= 100)
    fields(value['slots'], (), ('clock', 'date', 'status', 'link_code', 'approval', 'error'))
    for slot in value['slots'].values():
        fields(slot, ('x', 'y'), ('color', 'font_scale'))
        number(slot['x'], 0, 100)
        number(slot['y'], 0, 100)
        if 'color' in slot:
            color(slot['color'])
        if 'font_scale' in slot:
            number(slot['font_scale'], 0.5, 2)
    stage = value['stage']
    fields(stage, ('anchor', 'scale', 'background'))
    point(stage['anchor'])
    number(stage['scale'], 0, 2)
    check(stage['scale'] > 0)
    color(stage['background'])
    if 'css' in value:
        reference(files, value['css'], ('css',))


def clip(value, files):
    fields(value, ('mode',), ('webm', 'mp4'))
    check(value['mode'] in ('loop', 'once') and ('webm' in value or 'mp4' in value))
    for extension in ('webm', 'mp4'):
        if extension in value:
            reference(files, value[extension], (extension,))


def character(value, files):
    fields(value, ('rest',), ('states', 'emotes'))
    reference(files, value['rest'], ('png', 'webp'))
    states = value.get('states', {})
    fields(states, (), ('idle', 'connecting', 'listening', 'thinking', 'speaking',
                        'muted', 'setup', 'approval', 'error', 'wake', 'dnd', 'timer', 'call'))
    for state in states.values():
        fields(state, ('clip',), ('enter', 'exit'))
        for item in state.values():
            clip(item, files)
    emotes = value.get('emotes', {})
    check(type(emotes) is dict)
    for name, item in emotes.items():
        check(1 <= len(name) <= 128)
        clip(item, files)


def css_declaration(key, value, files):
    check(bool(value), 'unsafe or unsupported pack CSS')
    if key in ('color', 'background-color', 'border-color'):
        color(value)
    elif key == 'background-image':
        check(value.startswith('url(') and value.endswith(')'), 'unsafe or unsupported pack CSS')
        path = value[4:-1].strip()
        if path.startswith(('"', "'")):
            check(len(path) >= 2 and path[-1] == path[0], 'unsafe or unsupported pack CSS')
            path = path[1:-1]
        reference(files, path, ('png', 'webp'))
    elif key in ('font-weight', 'text-align'):
        allowed = ('normal', 'bold', '400', '500', '600', '700') if key == 'font-weight' else ('left', 'center', 'right')
        check(value in allowed, 'unsafe or unsupported pack CSS')
    elif key == 'opacity':
        number(float(value), 0, 1)
    elif key in ('font-size', 'border-radius', 'border-width', 'padding', 'gap'):
        match = re.fullmatch(r'(.+?)(px|rem|em|%)', value)
        check(match is not None, 'unsafe or unsupported pack CSS')
        number(float(match[1]), 0, 100)
    else:
        raise Refused('unsafe or unsupported pack CSS')


def stylesheet(data, files):
    text = data.decode('ascii').strip()
    check(not any(token in text for token in ('\\', '@', '<', '>', '/*', '*/')),
          'unsafe or unsupported pack CSS')
    while text:
        selectors, opening, rest = text.partition('{')
        body, closing, text = rest.partition('}')
        check(opening and closing, 'unsafe or unsupported pack CSS')
        allowed = {'.pack-' + name for name in ('clock', 'date', 'status', 'link-code', 'approval', 'error', 'stage')}
        check(all(selector.strip() in allowed for selector in selectors.split(',')),
              'unsafe or unsupported pack CSS')
        for item in body.split(';'):
            if item.strip():
                key, colon, value = item.partition(':')
                check(colon, 'unsafe or unsupported pack CSS')
                css_declaration(key.strip(), value.strip(), files)
        text = text.strip()


def validate(files):
    check('pack.json' in files)
    pack = json.loads(files['pack.json'])
    fields(pack, ('format', 'name', 'version'), ('layout', 'character'))
    check(type(pack['format']) is int and pack['format'] == 1)
    check(type(pack['name']) is str and re.fullmatch(r'[a-z][a-z0-9-]{0,47}', pack['name']))
    check(type(pack['version']) is str and re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9]+(?:\.[A-Za-z0-9]+)*)?', pack['version']))
    check('layout' in pack or 'character' in pack)
    if 'layout' in pack:
        layout(pack['layout'], files)
    if 'character' in pack:
        character(pack['character'], files)
    for name, data in files.items():
        if name.endswith('.css'):
            stylesheet(data, files)
    return pack['name'] + '@' + pack['version']


ROOT = '/data/local/echo-bridge/home/screens'


def adb(*args):
    subprocess.run(['adb', *args], check=True, stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL, timeout=30)


def push(files, identity):
    # All shell substitutions below are fixed roots, a UUID, or validated identity.
    stage = ROOT + '/.push-' + uuid.uuid4().hex
    lock = ROOT + '/.push-lock'
    destination = ROOT + '/' + identity
    guard = '\n'.join("test ! -L '" + path + "' && test -d '" + path + "' || exit 1"
                      for path in ('/data', '/data/local', '/data/local/echo-bridge',
                                   '/data/local/echo-bridge/home'))
    guard += "\ntest ! -L '" + ROOT + "' || exit 1\n"
    guard += "test ! -e '" + ROOT + "' || test -d '" + ROOT + "' || exit 1\n"
    acquired = False
    try:
        adb('shell', guard + "umask 077\nmkdir -p '" + ROOT + "' && mkdir '" + lock + "'")
        acquired = True
        adb('shell', "test ! -e '" + destination + "' && test ! -L '" + destination
            + "' && mkdir '" + stage + "'")
        with tempfile.TemporaryDirectory(prefix='screen-pack-') as temporary:
            source = Path(temporary) / 'payload'
            for name, data in sorted(files.items()):
                parent = name.rpartition('/')[0]
                if parent:
                    adb('shell', "mkdir -p '" + stage + '/' + parent + "'")
                source.write_bytes(data)
                adb('push', str(source), stage + '/' + name)
        adb('shell', "set -e\nchown 9999:9999 '" + ROOT + "'\nchmod 700 '" + ROOT
            + "'\nchown -R 9999:9999 '" + stage + "'\nfind '" + stage
            + "' -type d -exec chmod 700 {} +\nfind '" + stage
            + "' -type f -exec chmod 600 {} +\nmv -T -n '" + stage + "' '" + destination
            + "'\ntest ! -d '" + stage + "'")
    finally:
        if acquired:
            # Only our unpredictable private staging directory and acquired lock.
            adb('shell', guard + "rm -rf '" + stage + "' && rmdir '" + lock + "'")


def interrupted(_signal, _frame):
    raise Refused('push interrupted')


def main():
    if len(sys.argv) != 3 or sys.argv[1] != 'push':
        print('Usage: sh scripts/echo-screen-pack.sh push <directory>', file=sys.stderr)
        return 2
    check(sys.platform == 'linux', 'Linux is required')
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    files = read_files(Path(sys.argv[2]))
    identity = validate(files)
    push(files, identity)
    print('screen-pack: pushed ' + identity)
    return 0


try:
    sys.exit(main())
except Refused as error:
    print('screen-pack: ' + str(error), file=sys.stderr)
    sys.exit(1)
except (OSError, ValueError, TypeError, RecursionError, subprocess.SubprocessError):
    print('screen-pack: validation or transport failed', file=sys.stderr)
    sys.exit(1)
PY
