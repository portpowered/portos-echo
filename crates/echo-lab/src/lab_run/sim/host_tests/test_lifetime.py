"""T3 real controller death matrix; opt-in requires Linux and a prebuilt pin."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest

from lifetime_support import Controller, ROOT

@unittest.skipUnless(os.environ.get('ECHO_HOST_LIFETIME_T3') == '1', 'explicit T3 invocation only')
class Lifetime(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if sys.platform != 'linux' or not hasattr(os, 'pidfd_open'):
            raise RuntimeError('mandatory Linux/pidfd prerequisite unavailable')
        subprocess.run(['sh', str(ROOT / 'scripts/hassmic-host.sh'), 'path'], check=True,
                       stdout=subprocess.PIPE)

    def killed(self, kind, sig):
        with tempfile.TemporaryDirectory(prefix='lifetime-t3-') as folder:
            owner = Controller(kind, Path(folder))
            try:
                owner.ready()
                owner.observe_death(sig)
            finally:
                owner.close()

    def test_test_term(self):
        self.killed('test', signal.SIGTERM)

    def test_test_kill(self):
        self.killed('test', signal.SIGKILL)

    def test_sim_term(self):
        self.killed('sim', signal.SIGTERM)

    def test_sim_kill(self):
        self.killed('sim', signal.SIGKILL)

    def test_capture_term(self):
        self.killed('capture', signal.SIGTERM)

    def test_capture_kill(self):
        self.killed('capture', signal.SIGKILL)

    def test_hup_and_int_unwind_each_controller(self):
        for kind in ('test', 'sim', 'capture'):
            for sig in (signal.SIGHUP, signal.SIGINT):
                with self.subTest(kind=kind, signal=sig):
                    self.killed(kind, sig)

    def test_isolation_and_restarted_generations(self):
        sentinel = subprocess.Popen(['cat'], stdin=subprocess.PIPE, stdout=subprocess.DEVNULL)
        try:
            with tempfile.TemporaryDirectory(prefix='lifetime-isolation-') as folder:
                root = Path(folder)
                (root / 'other').mkdir()
                other = Controller('sim', root / 'other')
                try:
                    other.ready()
                    for kind in ('test', 'sim'):
                        with self.subTest(kind=kind):
                            (root / kind).mkdir()
                            owner = Controller(kind, root / kind)
                            try:
                                owner.ready()
                                old_stage = owner.stage
                                old_ids = dict(owner.owned)
                                if kind == 'test':
                                    owner.proc.stdin.write(b'stop\nstop\nrestart\n')
                                    owner.proc.stdin.flush()
                                    self.assertEqual([owner.line() for _ in range(3)], ['stop', 'stop', 'restart'])
                                else:
                                    owner.proc.stdin.write(b'{"action":"restart"}\n')
                                    owner.proc.stdin.flush()
                                    self.assertEqual(json.loads(owner.line())['event'], 'host.ready')
                                    ack = json.loads(owner.line())
                                    self.assertEqual(ack['event'], 'host.ack')
                                    self.assertNotEqual(ack['pid'], ack['previous_pid'])
                                self.assertFalse(old_stage.exists())
                                from lifetime_support import live
                                self.assertFalse(any(live(pid, start) for pid, start in old_ids.items()))
                                owner.identify()
                                self.assertNotEqual(owner.stage, old_stage)
                                owner.describe()
                                owner.observe_death(signal.SIGKILL)
                                other.describe()
                                self.assertIsNone(sentinel.poll())
                            finally:
                                owner.close()
                finally:
                    other.close()
        finally:
            sentinel.stdin.close()
            self.assertEqual(sentinel.wait(timeout=2), 0)

    def test_eof_exact_existing_transcript_and_cleanup(self):
        for kind in ('test', 'sim'):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory(prefix='lifetime-eof-') as folder:
                owner = Controller(kind, Path(folder))
                try:
                    owner.ready()
                    owner.proc.stdin.close()
                    self.assertEqual(owner.proc.wait(timeout=5), 0)
                    self.assertEqual(owner.line(), 'cleaned' if kind == 'test' else '{"event": "host.closed"}')
                    self.assertFalse(owner.stage.exists())
                    from lifetime_support import live, stage_pids
                    self.assertEqual(stage_pids(owner.stage), {})
                    self.assertFalse(any(live(pid, start) for pid, start in owner.owned.items()))
                finally:
                    owner.close()
