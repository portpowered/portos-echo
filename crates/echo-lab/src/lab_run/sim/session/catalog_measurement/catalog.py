"""Fixed inputs and byte-span accounting for the native session comparison."""
from collections import defaultdict
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import re


FILE_ENV = 'ECHO_LAB_TOOL_CATALOG_FILE'
CAPTURE_ENV = 'ECHO_LAB_TOOL_CATALOG_CAPTURE'
SNAPSHOT = '7b357af7fa19565d83ef32b0bbee535020b9499a5910879e4ae2baafdae27f23'
FIELDS = {'name', 'description', 'inputSchema'}
VARIANTS = ('flat', 'namespaced', 'deferred')
DOMAINS = {
    'endpoints': 'Discover, display, manage, and control Port OS endpoints.',
    'groups': 'Discover and manage Port OS device groups.',
    'subscriptions': 'List, inspect, cancel, and relabel Port OS event subscriptions.',
}


@dataclass(frozen=True)
class Case:
    id: str
    utterance: str
    expected: str


CASES = (
    Case('F01', 'List all the devices in my home.', 'endpoint_query'),
    Case('F02', 'Show the interactive card for the hallway lamp.', 'endpoint_show'),
    Case('F03', 'Rename the hallway lamp to Reading Lamp in the device inventory.', 'endpoint_manage'),
    Case('F04', 'Turn the hallway lamp off.', 'control'),
    Case('F05', 'Find the group named Downstairs.', 'group_query'),
    Case('F06', 'Rename the Downstairs group to Ground Floor.', 'group_manage'),
    Case('F07', 'List my event subscriptions.', 'subscription_list'),
    Case('F08', 'Inspect the current details for subscription alpha.', 'subscription_inspect'),
    Case('F09', 'Cancel subscription alpha.', 'subscription_cancel'),
    Case('F10', 'Change the label on subscription alpha to Morning alerts.', 'subscription_label_edit'),
)


def load(environ, digest=SNAPSHOT):
    """Absent variables explicitly select smoke. Any supplied input selects full mode."""
    if FILE_ENV not in environ and CAPTURE_ENV not in environ:
        names = ('endpoint_query', 'group_query')
        tools = [{'name': name, 'description': name.replace('_', ' '),
                  'inputSchema': {'type': 'object', 'properties': {}, 'additionalProperties': False}}
                 for name in names]
        return tools, None
    if not environ.get(FILE_ENV) or not environ.get(CAPTURE_ENV):
        raise ValueError(f'full replay requires both {FILE_ENV} and {CAPTURE_ENV}')
    try:
        raw = Path(environ[FILE_ENV]).read_bytes()
        tools = json.loads(raw)
    except (OSError, ValueError) as error:
        raise ValueError('full replay catalog is unreadable or malformed') from error
    if not valid(tools):
        raise ValueError('full replay requires one complete ten-tool projection')
    if hashlib.sha256(raw).hexdigest() != digest:
        raise ValueError('full replay catalog differs from the pinned projection')
    capture = Path(environ[CAPTURE_ENV])
    capture.mkdir(mode=0o700)
    return tools, capture


def valid(tools):
    return isinstance(tools, list) and len(tools) == len(CASES) and all(
        isinstance(tool, dict) and tool.keys() == FIELDS and isinstance(tool['name'], str)
        and isinstance(tool['description'], str) and bool(tool['description'])
        and isinstance(tool['inputSchema'], dict) and tool['inputSchema'].get('type') == 'object'
        for tool in tools
    ) and {tool['name'] for tool in tools} == {case.expected for case in CASES}


def namespace_for(name):
    stem = name.split('_', 1)[0]
    domain = {'group': 'groups', 'subscription': 'subscriptions'}.get(stem, 'endpoints')
    return f'portos_{domain}'


def declarations(tools, variant):
    direct = ['web', 'functions']
    settings = {'features.code_mode.direct_only_tool_namespaces': direct}
    if variant == 'flat':
        return tools, settings
    groups = defaultdict(list)
    for tool in tools:
        name = 'functions' if variant == 'forced-direct' else namespace_for(tool['name'])
        entry = {'type': 'function', **tool}
        if variant in ('deferred', 'forced-direct'):
            entry['deferLoading'] = True
        groups[name].append(entry)
    if variant == 'namespaced':
        direct.extend(groups)
    if variant == 'deferred':
        settings['features.code_mode.excluded_tool_namespaces'] = list(groups)
    return [{'type': 'namespace', 'name': name, 'tools': children,
             'description': DOMAINS.get(name.removeprefix('portos_'), 'Port OS tools.')}
            for name, children in groups.items()], settings


def intent(utterance):
    words = set(re.findall(r'[a-z]+', utterance.lower()))
    verb = utterance.split()[0].lower()
    if words & {'subscription', 'subscriptions'}:
        action = 'label_edit' if 'label' in words else verb
        return f'subscription_{action}'
    if 'group' in words:
        return 'group_manage' if verb == 'rename' else 'group_query'
    return {'turn': 'control', 'list': 'endpoint_query', 'show': 'endpoint_show',
            'rename': 'endpoint_manage'}.get(verb)


def elements(text, start, opening, closing):
    """Yield start/end offsets of values in an object or array at start."""
    decoder = json.JSONDecoder()
    whitespace = re.compile(r'[ \t\r\n]*')
    assert text[start] == opening, f'expected {opening}'
    position = whitespace.match(text, start + 1).end()
    while text[position] != closing:
        key = None
        if opening == '{':
            key, end = decoder.raw_decode(text, position)
            colon = whitespace.match(text, end).end()
            assert text[colon] == ':', 'missing object separator'
            position = whitespace.match(text, colon + 1).end()
        _, end = decoder.raw_decode(text, position)
        yield key, (position, end)
        position = whitespace.match(text, end).end()
        if text[position] == closing:
            break
        assert text[position] == ',', 'missing value separator'
        position = whitespace.match(text, position + 1).end()


def tool_spans(raw):
    text = raw.decode('utf-8')
    start = len(text) - len(text.lstrip())
    root = dict(elements(text, start, '{', '}'))
    for _, (begin, end) in elements(text, root['input'][0], '[', ']'):
        kind = json.loads(text[begin:end]).get('type')
        if kind in ('additional_tools', 'tool_search_output'):
            first, last = dict(elements(text, begin, '{', '}'))['tools']
            assert text[first] == '[', f'{kind} tools must be an array'
            yield kind, text[first:last]


def render(capture):
    import tiktoken
    from unittest.mock import patch

    if tiktoken.__version__ != '0.13.0':
        raise ValueError('full replay requires offline tiktoken 0.13.0')
    # The public constructor verifies the cached vocabulary hash. Refuse its
    # download fallback so a clean tokenizer cache cannot create remote traffic.
    with patch('tiktoken.load.read_file', side_effect=ValueError('offline o200k_base vocabulary is missing')):
        encoding = tiktoken.get_encoding('o200k_base')
    print('variant case tool initial followup_additional followup_search round_trips')
    rows = json.loads((capture / 'selections.json').read_bytes())
    assert [(row['variant'], row['case'], row['tool']) for row in rows] == [
        (variant, case.id, case.expected) for variant in VARIANTS for case in CASES
    ], 'incomplete or inconsistent full replay results'
    for row in rows:
        totals = [0, 0, 0]
        for index in range(row['round_trips']):
            filename = f"{row['variant']}-{row['case']}-{index}.json"
            for kind, span in tool_spans((capture / filename).read_bytes()):
                column = 2 if kind == 'tool_search_output' else int(index > 0)
                totals[column] += len(encoding.encode_ordinary(span))
        print(row['variant'], row['case'], row['tool'], *totals, row['round_trips'])
