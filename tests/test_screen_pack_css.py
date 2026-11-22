"""T2 parity witness: the push helper admits exactly the CSS the bridge admits.

The bridge validator (crates/echo-screen-server/src/packs/css.rs) is the rule
source. Its own test vectors run through scripts/echo-screen-pack.sh with a
fake adb, and the two property lists must stay equal. No network or device.
"""
import json
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_echo_install import require  # noqa: E402
from test_screen_pack_assets import PushBox, ROOT  # noqa: E402

BRIDGE_PACKS = ROOT / 'crates/echo-screen-server/src/packs'


def rust_arrays(source, function):
    """The string literals of each `for value in [...]` loop in one Rust test."""
    body = source.split(f'fn {function}()', 1)[1].split('\n}\n', 1)[0]
    return [[json.loads(literal) for literal in re.findall(r'"(?:[^"\\]|\\.)*"', array)]
            for array in re.findall(r'for value in \[(.*?)\n\s*\]', body, re.S)]


def css_cases():
    tests = (BRIDGE_PACKS / 'tests.rs').read_text()
    admitted, refused = rust_arrays(tests, 'pack_css_text_shadow_is_bounded')
    [selectors_refused] = rust_arrays(
        tests, 'pack_css_accepts_only_documented_selectors_declarations_and_local_files')
    require(len(admitted) >= 5 and len(refused) >= 20 and len(selectors_refused) >= 10,
            'bridge CSS vectors were not found')
    cases = [(f'.pack-clock, .pack-date {{ text-shadow: {value}; }}', True) for value in admitted]
    cases += [(f'.pack-clock {{ text-shadow: {value}; }}', False) for value in refused]
    cases += [(css, False) for css in selectors_refused]
    return cases + [
        ((ROOT / 'scripts/packs/companion/companion-clock.css').read_text(), True),
        ('.pack-clock { text-shadow: 0px 2px #000000; position: fixed; }', False),
        ('.pack-clock { text-shadow: 0px 2px #000000; } .pack-date { filter: blur(1px); }', False),
        ('.pack-clock { text-shadow: 1_0px 2px #000000; }', False),
        ('.pack-clock { font-size: 1_0px; }', False),
    ]


def bridge_properties():
    source = (BRIDGE_PACKS / 'css.rs').read_text()
    body = source.split('fn declaration_value(', 1)[1].split('\n}\n', 1)[0]
    arms = re.findall(r'^\s*((?:"[a-z-]+"\s*\|\s*)*"[a-z-]+")\s*(?:if\b|=>)', body, re.M)
    return set(re.findall(r'"([a-z-]+)"', ' '.join(arms)))


def helper_properties():
    listed = re.search(r'CSS_PROPERTIES = \((.*?)\)',
                       (ROOT / 'scripts/echo-screen-pack.sh').read_text(), re.S)
    return set(re.findall(r"'([a-z-]+)'", listed[1])) if listed else set()


def css_parity_cases():
    cases = css_cases()
    for css, okay in cases:
        with PushBox() as box:
            (box.pack / 'rest.png').write_bytes(b'synthetic-still')
            (box.pack / 'style.css').write_text(css)
            try:
                box.run(okay=okay)
            except AssertionError as error:
                verdict = 'admits' if okay else 'refuses'
                raise AssertionError(f'{error} (bridge {verdict} {css!r})') from None
            require(okay or box.ledger() == [], 'bridge-refused CSS reached adb: ' + css)
    bridge, helper = bridge_properties(), helper_properties()
    require(len(bridge) >= 12 and helper == bridge,
            f'helper CSS properties {sorted(helper)} differ from bridge {sorted(bridge)}')
    print(f'F24 css parity: vectors={len(cases)} properties={len(bridge)} helper=bridge')


if __name__ == '__main__':
    css_parity_cases()
