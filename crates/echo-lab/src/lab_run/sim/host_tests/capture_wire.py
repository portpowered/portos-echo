"""Offline capture wire/transaction cases, loaded by capture --self-test."""
class WireTests(unittest.TestCase):
    def test_fragmented_utf8_and_payload(self):
        class Fragmented(io.BytesIO):
            def read(self, n=-1):
                return super().read(min(n, 1))
        data = {"text": "\u00e9"}
        payload = bytes(range(80))
        wire = encoded({"type": "test", "data_length": len(encoded(data)), "payload_length": 80})
        event, actual = read_event(Fragmented(wire + b"\n" + encoded(data) + payload))
        self.assertEqual(event, {"type": "test", "data": data})
        self.assertEqual(actual, payload)
        row = record("echo->bridge", event, actual)
        self.assertEqual(row["payload_sha256"], hashlib.sha256(payload).hexdigest())
        self.assertEqual(base64.b64decode(row["payload_prefix_b64"]), payload[:32])
    def test_header_only_inline_and_invalid_lengths(self):
        self.assertEqual(read_event(io.BytesIO(b'{"type":"pong"}\n')), ({"type": "pong", "data": {}}, b""))
        self.assertEqual(read_event(io.BytesIO(b'{"type":"x","data":{"a":1}}\n'))[0]["data"], {"a": 1})
        for n in (-1, 1048577, True, 1.5):
            with self.assertRaises(ValueError):
                read_event(io.BytesIO(encoded({"type": "x", "payload_length": n}) + b"\n"))
        for raw in (b"", b'{"type":"x","data_length":2}\n{', b'{"type":"x","payload_length":2}\nx'):
            with self.assertRaises(EOFError):
                read_event(io.BytesIO(raw))
    def test_normalization_and_zero_payload(self):
        value = {"timestamp": 123, "satellite": {"version": "volatile"}}
        self.assertEqual(normalize(value), {"timestamp": 0, "satellite": {"version": "<version>"}})
        self.assertEqual(normalize(normalize(value)), normalize(value))
        self.assertEqual(record("echo->bridge", {"type": "pong", "data": {}}),
                         {"dir": "echo->bridge", "header": {"type": "pong", "data": {}, "data_length": 0, "payload_length": 0}})
    def test_publication_byte_maps(self):
        rows = {k: [json.loads(line) for line in raw.splitlines()]
                for k, raw in fixture_bytes().items() if k[:-6] in SCENARIOS}
        fresh = {k + ".jsonl": b"".join(encoded(r) + b"\n" for r in rows[k + ".jsonl"]) for k in SCENARIOS}
        def producer(kind, _):
            return rows[kind + ".jsonl"]
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "fixtures"
            publish(output, producer)
            self.assertEqual(fixture_bytes(output), fresh)
            publish(output, producer)
            self.assertEqual(fixture_bytes(output), fresh)
            (output / "nested").mkdir()
            (output / "nested/unrelated").write_bytes(b"sibling")
            for name, raw in fixture_bytes().items():
                (output / name).write_bytes(b"stale" if name in fresh else raw)
            old = fixture_bytes(output)
            for failure in (EOFError, TimeoutError, InterruptedError, KeyboardInterrupt):
                def failed(kind, folder):
                    if kind == SCENARIOS[1]:
                        raise failure("capture interrupted")
                    return producer(kind, folder)
                with self.assertRaises(failure):
                    publish(output, failed)
                self.assertEqual(fixture_bytes(output), old)
                self.assertEqual(list(Path(temp).iterdir()), [output])
            with publication(output) as lock:
                (lock / "owner").write_bytes(b"owner")
                with self.assertRaises(FileExistsError):
                    publish(output, lambda *_: self.fail("contender called producer"))
                self.assertEqual((lock / "owner").read_bytes(), b"owner")
                self.assertEqual(fixture_bytes(output), old)
                (lock / "owner").unlink()
            rename, mask = Path.rename, signal.pthread_sigmask
            def fail_rename(path, target):
                if path.name == "fixtures" and path.parent != output.parent:
                    raise OSError("stage rename failed")
                return rename(path, target)
            def cancel(how, signals):
                result = mask(how, signals)
                if how == signal.SIG_SETMASK:
                    raise InterruptedError("pending cancellation")
                return result
            for target, effect in (("shutil.copytree", OSError("copy failed")),
                                   ("pathlib.Path.write_bytes", OSError("write failed")),
                                   ("pathlib.Path.rename", fail_rename),
                                   ("signal.pthread_sigmask", cancel)):
                with mock.patch(target, side_effect=effect, autospec=True), self.assertRaises(OSError):
                    publish(output, producer)
                self.assertEqual(fixture_bytes(output), old)
                self.assertEqual(list(Path(temp).iterdir()), [output])
            publish(output, producer)
            self.assertEqual(fixture_bytes(output), {**old, **fresh})
            self.assertEqual(list(Path(temp).iterdir()), [output])
    def test_flattened_or_extra_header_fields_fail(self):
        row = record("echo->bridge", {"type": "info", "data": {}})
        for changed in ({"dir": row["dir"], **row["header"]},
                        {**row, "type": "info"},
                        {**row, "header": {**row["header"], "payload": "forbidden"}}):
            with self.assertRaises(ValueError):
                validate([changed], "describe")
    def test_microphone_start_and_interrupted_wake_sequence_fail(self):
        output = ROOT / "tests/fixtures/wyoming"
        for kind in SCENARIOS[1:]:
            rows = [json.loads(line) for line in (output / (kind + ".jsonl")).read_bytes().splitlines()]
            pipeline = next(i for i, row in enumerate(rows) if row["header"]["type"] == "run-pipeline")
            for kind_added in ("audio-start", "pong"):
                changed = rows[:pipeline + 1] + [record("echo->bridge", {"type": kind_added, "data": {}})] + rows[pipeline + 1:]
                with self.subTest(scenario=kind, inserted=kind_added), self.assertRaises(AssertionError):
                    validate(changed, kind)
