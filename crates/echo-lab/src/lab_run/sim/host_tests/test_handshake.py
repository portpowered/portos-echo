"""Fixture T3: controlled Host/observer boundary, real children and pipe readers.

Unique processes and directories per case; signal gates, never timing sleeps.
These faults prove fixture ownership, not real WSL or pinned-host readiness.
"""
import contextlib
import json
import os
import runpy
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock

from test_controls import CONTROLLER, real_wsl_launcher_fixture
from owner import instrument

POPEN = subprocess.Popen


@unittest.skipUnless(hasattr(os, "pidfd_open"), "Linux pidfd ownership observer")
class ListenerOwner(unittest.TestCase):
    def test_t3_pidfd_rejects_a_real_retained_listener_despite_a_closed_flag(self):
        for leak in [False, True]:
            with self.subTest(leak=leak), tempfile.TemporaryDirectory(prefix="lab-owner-") as folder:
                events = []
                globals()["emit"] = events.append
                self.addCleanup(lambda: globals().pop("emit", None))

                class Host:
                    def start(self):
                        code = ("import socket,sys; s=socket.socket(); s.bind(('127.0.0.1',0)); "
                                "s.listen(); print(s.getsockname()[1],flush=True); sys.stdin.buffer.read()")
                        self.proc = POPEN([sys.executable, "-u", "-c", code], stdin=subprocess.PIPE,
                                          stdout=subprocess.PIPE)
                        self.port = int(self.proc.stdout.readline())
                        emit({"event": "host.ready", "address": f"127.0.0.1:{self.port}"})

                    def close(self):
                        self.closed = True
                        if not leak:
                            self.proc.stdin.close()
                            self.proc.wait(timeout=4)

                audit = Path(folder) / "host.json"
                instrument({"Host": Host}, audit)
                host = Host()
                try:
                    host.start()
                    self.assertEqual(events, [{"event": "host.ready", "address": f"127.0.0.1:{host.port}"}])
                    host.close()
                    observed = json.loads(audit.read_text())
                    self.assertEqual(observed["owner_completed"], not leak)
                    self.assertTrue(host.closed)
                    self.assertIsNotNone(host.proc.returncode)
                finally:
                    if host.proc.poll() is None:
                        host.proc.kill()
                    host.proc.wait(timeout=4)
                    host.proc.stdin.close()
                    host.proc.stdout.close()


class FixtureHandshake(unittest.TestCase):
    def setUp(self):
        self.events = []
        self.children = []
        self.started = threading.Event()
        self.spawned = threading.Event()
        self.release = threading.Event()
        self.failure = []
        self.mode = "ready"
        self.directory = tempfile.TemporaryDirectory(prefix="lab-handshake-")
        self.addCleanup(self.directory.cleanup)
        case = self

        class Host:
            def __init__(self):
                self.condition = threading.Condition()
                self.cancelled = threading.Event()
                self.proc = None
                self.observer = None
                self.observer_reader = None
                self.closed = False

            def start(self):
                if case.mode == "prerequisite":
                    raise OSError("synthetic private marker")
                code = "import sys; sys.stdin.buffer.read()"
                if case.mode == "guest-exit":
                    code = "import sys; sys.exit(7)"
                self.proc = case.child(code)
                case.started.set()
                with self.condition:
                    ready = self.condition.wait_for(
                        lambda: case.release.is_set() or self.cancelled.is_set(), 20)
                if case.mode == "guest-exit":
                    self.proc.wait(timeout=1)
                    raise RuntimeError("synthetic private marker")
                if not ready or self.cancelled.is_set():
                    raise RuntimeError("synthetic private marker")
                case.events.append({"event": "backend.ready"})
                emit({"event": "host.ready", "address": "127.0.0.1:1"})

            def close(self):
                if self.proc and not self.closed:
                    self.proc.stdin.close()
                    self.proc.wait(timeout=1)
                self.closed = True

            def logged(self, marker):
                pass

        # start's globals are this module, while the fixture captures emit exactly
        # as it does from the real Host. Restore the module after each case.
        self.addCleanup(lambda: globals().pop("emit", None))
        globals()["emit"] = self.events.append
        with mock.patch.object(runpy, "run_path", return_value={"Host": Host}), \
                mock.patch.object(sys, "argv", ["fixture", CONTROLLER.__file__, "owned"]):
            self.namespace = {"__name__": "instrumentation"}
            exec(real_wsl_launcher_fixture(), self.namespace)
        self.host = Host()
        self.worker = None
        self.addCleanup(self.cleanup)

    def child(self, code):
        proc = POPEN([sys.executable, "-u", "-c", code], stdin=subprocess.PIPE,
                     stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.children.append(proc)
        return proc

    def observer(self, *args, **kwargs):
        if self.mode == "observer-spawn":
            raise OSError("synthetic private marker")
        code = ("import sys; print('entered',file=sys.stderr,flush=True); "
                "release=sys.stdin.buffer.readline(); "
                "sys.stdout.buffer.write(b'observer.ready\\n') if release else None; "
                "sys.stdout.buffer.flush(); sys.stdin.buffer.read()")
        if self.mode == "observer-exit":
            code = "import sys; sys.exit(7)"
        if self.mode == "observer-stall":
            code = "import threading; threading.Event().wait()"
        self.observer_proc = self.child(code)
        self.spawned.set()
        return self.observer_proc

    def begin(self):
        def start():
            try:
                self.host.start()
            except RuntimeError as error:
                self.failure.append(str(error))
        self.worker = threading.Thread(target=start)
        self.worker.start()

    def unblock(self, cancel=False):
        with self.host.condition:
            (self.host.cancelled if cancel else self.release).set()
            self.host.condition.notify_all()

    def cleanup(self):
        self.unblock(cancel=True)
        if self.worker:
            self.worker.join(timeout=1)
        with mock.patch.object(Path, "write_text"), contextlib.suppress(RuntimeError):
            self.host.close()
        for proc in self.children:
            if proc.poll() is None:
                proc.kill()
            proc.wait(timeout=1)
            for pipe in [proc.stdin, proc.stdout, proc.stderr]:
                if pipe and not pipe.closed:
                    pipe.close()

    def assert_closed(self):
        with mock.patch.object(Path, "write_text") as audit:
            self.host.close()
            self.host.close()
        self.assertTrue(self.host.closed)
        self.assertTrue(all(proc.poll() is not None for proc in self.children))
        if self.host.proc:
            self.assertIn('"closed": true', audit.call_args.args[0])
        reader = self.host.observer_reader
        self.assertTrue(reader is None or not reader.is_alive())

    def test_t3_signal_released_ready_waits_for_owned_observer(self):
        with mock.patch.object(subprocess, "Popen", side_effect=self.observer):
            self.begin()
            self.assertTrue(self.started.wait(1))
            self.assertEqual(self.events, [])
            self.assertFalse(self.spawned.is_set())
            self.unblock()
            self.assertTrue(self.spawned.wait(1))
            self.assertEqual(self.observer_proc.stderr.readline(), b"entered\r\n" if sys.platform == "win32" else b"entered\n")
            self.assertEqual(self.events, [{"event": "backend.ready"}])
            self.observer_proc.stdin.write(b"R\n")
            self.observer_proc.stdin.flush()
            self.worker.join(timeout=1)
            self.assertFalse(self.worker.is_alive())
        self.assertEqual(self.failure, [])
        self.assertEqual([v["event"] for v in self.events], ["backend.ready", "host.ready", "guest.owned"])
        self.assertEqual(self.events[-1]["pid"], self.host.proc.pid)
        self.assert_closed()

    def cancelled_setup(self, stage):
        if stage == "observer":
            self.release.set()
        with mock.patch.object(subprocess, "Popen", side_effect=self.observer):
            self.begin()
            self.assertTrue((self.spawned if stage == "observer" else self.started).wait(1))
            self.unblock(cancel=True)
            self.worker.join(timeout=1)
            self.assertFalse(self.worker.is_alive())
        self.assertEqual(self.failure, ["fixture setup failed"])
        self.assertEqual(self.events[-1], {"event": "fixture.failed", "stage": "guest.cancelled"})
        self.assertNotIn("host.ready", [v["event"] for v in self.events])
        self.assert_closed()

    def test_t3_cancel_readiness(self):
        self.cancelled_setup("readiness")

    def test_t3_cancel_observer_reader(self):
        self.cancelled_setup("observer")

    def failed_setup(self, mode, stage):
        self.mode = mode
        self.release.set()
        with mock.patch.object(subprocess, "Popen", side_effect=self.observer):
            with self.assertRaisesRegex(RuntimeError, "^fixture setup failed$"):
                self.host.start()
        self.assertEqual(self.events[-1], {"event": "fixture.failed", "stage": "guest." + stage})
        self.assertNotIn("host.ready", [v["event"] for v in self.events])
        if mode == "guest-exit":
            self.assertEqual(self.host.proc.returncode, 7)
        if mode == "observer-exit":
            with mock.patch.object(Path, "write_text"):
                with self.assertRaisesRegex(RuntimeError, "^fixture observer exited$"):
                    self.host.close()
            self.assertEqual(self.observer_proc.returncode, 7)
            self.assertFalse(self.host.observer_reader.is_alive())
            self.assertTrue(self.observer_proc.stdout.closed)
            self.assertIsNotNone(self.host.proc.returncode)
        else:
            self.assert_closed()

    def test_t3_unavailable_prerequisite(self):
        self.failed_setup("prerequisite", "readiness")

    def test_t3_guest_exit(self):
        self.failed_setup("guest-exit", "readiness")

    def test_t3_observer_spawn_failure(self):
        self.failed_setup("observer-spawn", "observer")

    def test_t3_observer_exit(self):
        self.failed_setup("observer-exit", "observer")

    def test_controlled_readiness_ceiling_never_starts_observer(self):
        self.release.set()
        with mock.patch.object(self.host.condition, "wait_for", return_value=False) as wait, \
                mock.patch.object(subprocess, "Popen", side_effect=self.observer):
            with self.assertRaisesRegex(RuntimeError, "^fixture setup failed$"):
                self.host.start()
        self.assertEqual(wait.call_args.args[1], 20)
        self.assertFalse(self.spawned.is_set())
        self.assertEqual(self.events, [{"event": "fixture.failed", "stage": "guest.readiness"}])
        self.assert_closed()

    def test_controlled_observer_ceiling_uses_remaining_budget_and_collects_reader(self):
        self.release.set()
        original = self.host.condition.wait_for
        def wait(predicate, timeout):
            if timeout == 0:
                return False
            return original(predicate, timeout)
        with mock.patch.object(self.host.condition, "wait_for", side_effect=wait) as waited, \
                mock.patch.object(subprocess, "Popen", side_effect=self.observer), \
                mock.patch.object(self.namespace["time"], "monotonic", side_effect=[100, 120]):
            with self.assertRaisesRegex(RuntimeError, "^fixture setup failed$"):
                self.host.start()
        self.assertEqual([call.args[1] for call in waited.call_args_list], [20, 0])
        self.assertEqual(self.events, [{"event": "backend.ready"},
                                     {"event": "fixture.failed", "stage": "guest.observer"}])
        self.assert_closed()

    def test_unavailable_translation_reports_stage_through_fixture_entry(self):
        with mock.patch.object(CONTROLLER, "emit") as emitted, \
                mock.patch.object(subprocess, "Popen", side_effect=OSError("synthetic private marker")):
            with self.assertRaises(SystemExit) as exited:
                real_wsl_launcher_fixture("owned")
        self.assertEqual(exited.exception.code, 1)
        emitted.assert_called_once_with({"event": "fixture.failed", "stage": "translation"})

    def test_t3_reader_start_failure_collects_acquired_observer(self):
        self.release.set()
        with mock.patch.object(subprocess, "Popen", side_effect=self.observer), \
                mock.patch.object(threading.Thread, "start", side_effect=RuntimeError("synthetic private marker")):
            with self.assertRaisesRegex(RuntimeError, "^fixture setup failed$"):
                self.host.start()
        self.assertEqual(self.events[-1], {"event": "fixture.failed", "stage": "guest.observer"})
        with mock.patch.object(Path, "write_text"):
            self.host.close()
        self.assertEqual(self.observer_proc.returncode, 0)
        self.assertIsNotNone(self.host.proc.returncode)
        self.assertTrue(self.observer_proc.stdout.closed)

    def test_t3_cancel_cannot_mask_forced_observer_cleanup_failure(self):
        self.mode = "observer-stall"
        self.release.set()
        with mock.patch.object(subprocess, "Popen", side_effect=self.observer):
            self.begin()
            self.assertTrue(self.spawned.wait(1))
            self.unblock(cancel=True)
            self.worker.join(timeout=1)
        self.assertEqual(self.failure, ["fixture setup failed"])
        with self.assertRaisesRegex(RuntimeError, "^fixture observer cleanup failed$"):
            self.host.close()
        status = self.observer_proc.returncode
        self.assertNotEqual(status, 0)
        with self.assertRaisesRegex(RuntimeError, "^fixture observer exited$"):
            self.host.close()
        self.assertEqual(self.observer_proc.returncode, status)
        self.assertFalse(self.host.observer_reader.is_alive())
        self.assertTrue(self.observer_proc.stdout.closed)
        self.assertIsNotNone(self.host.proc.returncode)
