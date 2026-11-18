"""T1 hook, fork-race refusal and controller unwind decisions; no real signals."""
import ctypes
import importlib.util
import os
from pathlib import Path
import signal
import sys
import tempfile
import unittest
from unittest import mock

from test_controls import CONTROLLER

ROOT = Path(__file__).resolve().parents[6]
def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

CAPTURE = load('capture_guard', ROOT / 'scripts/capture-wyoming.py')
MODULES = [CONTROLLER, CAPTURE]
if sys.platform == 'linux':
    MODULES.append(load('wyoming_guard', ROOT / 'tests/wyoming_host.py'))

class Guard(unittest.TestCase):
    def setUp(self):
        patch = mock.patch.object(signal, "SIGKILL", 9, create=True)
        patch.start()
        self.addCleanup(patch.stop)

    def test_guard_arms_exact_sigkill_before_parent_check(self):
        for module in MODULES:
            with self.subTest(module=module.__name__):
                libc = mock.Mock()
                libc.prctl.return_value = 0
                events = mock.Mock()
                events.attach_mock(libc.prctl, 'prctl')
                with mock.patch.object(ctypes, 'CDLL', return_value=libc) as dll, \
                        mock.patch.object(module.os, 'getpid', return_value=123), \
                        mock.patch.object(module.os, 'getppid', return_value=123) as parent, \
                        mock.patch.object(module.os, 'kill') as kill:
                    events.attach_mock(parent, 'parent')
                    arm = module.parent_death_guard()
                    dll.assert_called_once_with(None, use_errno=True)
                    libc.prctl.assert_not_called()
                    arm()
                    self.assertEqual(events.mock_calls, [mock.call.prctl(1, 9, 0, 0, 0), mock.call.parent()])
                    kill.assert_not_called()
                    parent.return_value = 1
                    arm()
                    kill.assert_called_once_with(123, 9)

    def test_prctl_refusal_never_proceeds_to_exec_or_parent_check(self):
        for module in MODULES:
            libc = mock.Mock()
            libc.prctl.return_value = -1
            with self.subTest(module=module.__name__), \
                    mock.patch.object(ctypes, 'CDLL', return_value=libc), \
                    mock.patch.object(ctypes, 'get_errno', return_value=1), \
                    mock.patch.object(module.os, 'getppid') as parent, \
                    mock.patch.object(module.os, 'kill') as kill:
                with self.assertRaises(PermissionError):
                    module.parent_death_guard()()
                parent.assert_not_called()
                kill.assert_not_called()

    def test_handled_signals_unwind_ignore_repeats_then_restore(self):
        for module in MODULES:
            for delivered in (15, 1, 2):
                with self.subTest(module=module.__name__, signal=delivered), \
                        mock.patch.object(module.os, 'name', 'posix'), \
                        mock.patch.multiple(signal, SIGTERM=15, SIGHUP=1, SIGINT=2, create=True), \
                        mock.patch.object(signal, 'getsignal', return_value='previous'), \
                        mock.patch.object(signal, 'signal') as install:
                    cleaned = []
                    with self.assertRaisesRegex(SystemExit, f'cancelled by signal {delivered}'):
                        with module.controller_signals():
                            handler = install.call_args_list[0].args[1]
                            try:
                                handler(delivered, None)
                            finally:
                                cleaned.append('collected')
                                self.assertEqual(install.call_args_list[-3:],
                                                 [mock.call(sig, signal.SIG_IGN) for sig in (15, 1, 2)])
                    self.assertEqual(cleaned, ['collected'])
                    self.assertEqual(install.call_args_list[-3:],
                                     [mock.call(sig, 'previous') for sig in (15, 1, 2)])

    @unittest.skipUnless(sys.platform == 'linux', 'Linux staging ownership')
    def test_stage_and_exec_failures_never_emit_ready_and_release_owned_resources(self):
        for module, cls in ((CONTROLLER, CONTROLLER.Host), (CAPTURE, CAPTURE.Session)):
            for fault in ('hash', 'exec', 'prctl'):
                with self.subTest(module=module.__name__, fault=fault), tempfile.TemporaryDirectory() as folder:
                    owner = cls(folder)
                    stages = []
                    original_stage = owner.stage
                    def stage():
                        # The real stage acquisition and hash verification still run.
                        try:
                            return original_stage()
                        finally:
                            if owner.staged:
                                stages.append(Path(owner.staged.name))
                    pinned = Path(folder) / 'pin'
                    pinned.write_bytes(b'prebuilt test input')
                    def copy(_source, destination):
                        destination.write_bytes(b'bad copy' if fault == 'hash' else pinned.read_bytes())
                    def launch(*args, **kwargs):
                        if fault == 'prctl':
                            raise subprocess.SubprocessError('Exception occurred in preexec_fn')
                        raise FileNotFoundError('controlled exec failure')
                    import subprocess
                    with mock.patch.object(owner, 'stage', side_effect=stage), \
                            mock.patch.object(module.subprocess, 'check_output', return_value=str(pinned)), \
                            mock.patch.object(module.shutil, 'copy2', side_effect=copy), \
                            mock.patch.object(module.socket, 'socket'), \
                            mock.patch.object(module.threading, 'Thread'), \
                            mock.patch.object(module.subprocess, 'Popen', side_effect=launch), \
                            mock.patch.object(module, 'emit', create=True) as emit:
                        with self.assertRaises((RuntimeError, OSError, subprocess.SubprocessError)):
                            if module is CAPTURE:
                                owner.__enter__()
                            else:
                                try:
                                    owner.start()
                                finally:
                                    owner.close()
                        emit.assert_not_called()
                    self.assertIsNone(owner.proc)
                    self.assertIsNone(owner.staged)
                    self.assertEqual(len(stages), 1)
                    self.assertEqual([stage for stage in stages if stage.exists()], [])
                    if module is CONTROLLER:
                        self.assertIsNone(owner.fd)
                        self.assertIsNone(owner.keys)
                    elif owner.feeder:
                        owner.feeder.join.assert_called_once_with(timeout=1)
