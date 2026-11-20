"""Network-free tests for full-mode admission and the observed byte spans."""
import json
from pathlib import Path
import tempfile
import unittest

from catalog import CAPTURE_ENV, CASES, FILE_ENV, intent, load, tool_spans


class CatalogInput(unittest.TestCase):
    def test_absent_inputs_only_select_smoke(self):
        tools, capture = load({})
        self.assertEqual([tool['name'] for tool in tools], ['endpoint_query', 'group_query'])
        self.assertIsNone(capture)
        for env in ({FILE_ENV: ''}, {CAPTURE_ENV: 'unused'}, {FILE_ENV: 'catalog'}):
            with self.subTest(env=env), self.assertRaisesRegex(ValueError, 'requires both'):
                load(env)

    def test_full_rejects_missing_malformed_and_stale_projections(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / 'catalog.json'
            capture = Path(directory) / 'capture'
            env = {FILE_ENV: str(source), CAPTURE_ENV: str(capture)}
            with self.assertRaisesRegex(ValueError, 'unreadable or malformed'):
                load(env)
            source.write_text('{')
            with self.assertRaisesRegex(ValueError, 'unreadable or malformed'):
                load(env)
            source.write_text('[]')
            with self.assertRaisesRegex(ValueError, 'complete ten-tool projection'):
                load(env)
            tools = [{'name': case.expected, 'description': 'synthetic', 'inputSchema': {'type': 'object'}}
                     for case in CASES]
            source.write_text(json.dumps(tools))
            with self.assertRaisesRegex(ValueError, 'differs from the pinned projection'):
                load(env)
            self.assertFalse(capture.exists())

    def test_fixture_routing(self):
        self.assertEqual([intent(case.utterance) for case in CASES], [case.expected for case in CASES])


class Capture(unittest.TestCase):
    def test_counts_use_the_original_spelling_and_input_offsets(self):
        additional = '[ {"description": "é [ x ] \\"", "tools":[1]} ]'
        search = '[{"name":"\\u00e9"}]'
        raw = (' {"metadata":{"input":[]}, "input":[{"type":"message","tools":[]},'
               f'{{"tools": {additional}, "type": "additional_tools"}},'
               f'{{"type": "tool_search_output", "tools":{search}}} ]}}').encode()
        self.assertEqual(list(tool_spans(raw)), [('additional_tools', additional), ('tool_search_output', search)])
