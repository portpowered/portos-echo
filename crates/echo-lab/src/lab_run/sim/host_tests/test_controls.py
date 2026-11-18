"""T1 fake-process controls and explicitly named T3 real process-group ownership."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("sim_host", Path(__file__).resolve().parents[1] / "host.py")
CONTROLLER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CONTROLLER)
class Controls(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="lab-host-unit-")
        self.addCleanup(self.directory.cleanup)
        self.host = CONTROLLER.Host(self.directory.name)
        self.host.proc = mock.Mock()
    def test_supported_controls_send_exact_signal_once(self):
        signals = {"SIGUSR1": 10, "SIGUSR2": 12, "SIGWINCH": 28, "SIGHUP": 1}
        with mock.patch.multiple(CONTROLLER.signal, create=True, **signals), mock.patch.object(CONTROLLER, "emit") as emit:
            for action, expected in [("wake", 10), ("action", 12), ("pair", 28), ("stop", 1)]:
                self.host.proc.reset_mock()
                emit.reset_mock()
                self.host.command({"action": action})
                self.host.proc.send_signal.assert_called_once_with(expected)
                emit.assert_called_once_with({"event": "host.ack"})
    def test_registration_waits_for_log_without_sending_a_signal(self):
        with mock.patch.object(self.host, "logged") as logged, mock.patch.object(CONTROLLER, "emit") as emit:
            self.host.command({"action": "registered"})
            logged.assert_called_once_with(b"satellite: running")
            self.host.proc.send_signal.assert_not_called()
            emit.assert_called_once_with({"event": "host.ack"})
    def test_eof_interrupts_readiness_and_rejects_further_commands(self):
        events = CONTROLLER.queue.Queue()
        with mock.patch.object(CONTROLLER, "launcher_input") as reader:
            reader.return_value = True
            CONTROLLER.host_input(self.host, events)
            reader.assert_called_once_with(events)
        for marker in [b"wyoming on ", b"satellite: running"]:
            with self.assertRaises(CONTROLLER.HostCancelled):
                self.host.logged(marker)
        with self.assertRaisesRegex(RuntimeError, "host command cancelled"):
            self.host.command({"action": "wake"})
        self.host.proc.send_signal.assert_not_called()
        with mock.patch.object(CONTROLLER, "launcher_input", return_value=False):
            CONTROLLER.host_input(self.host, events)
        with self.assertRaisesRegex(RuntimeError, "host input failed"):
            self.host.command({"action": "wake"})
    def test_unsupported_controls_and_invalid_pcm_cannot_signal(self):
        # Inject explicit signals without starting the host backend.
        with mock.patch.multiple(CONTROLLER.signal, create=True, SIGUSR1=10, SIGUSR2=12, SIGWINCH=28, SIGHUP=1):
            with self.assertRaises(KeyError):
                self.host.command({"action": "unknown"})
            for pcm in [[], [1], [0] * 4098]:
                with self.assertRaises(ValueError):
                    self.host.command({"action": "say", "pcm": pcm})
            self.host.proc.send_signal.assert_not_called()
class Cleanup(unittest.TestCase):
    def setUp(self):
        Controls.setUp(self)
        patch = mock.patch.object(CONTROLLER.signal, "SIGKILL", 9, create=True)
        patch.start()
        self.addCleanup(patch.stop)
    def test_graceful_group_signal_precedes_wait_and_is_idempotent(self):
        self.host.proc.poll.return_value = None
        with mock.patch.object(CONTROLLER.os, "killpg", create=True) as signal:
            self.host.close()
            self.host.close()
            signal.assert_called_once_with(self.host.proc.pid, CONTROLLER.signal.SIGTERM)
            self.assertEqual(self.host.proc.wait.call_args_list, [mock.call(timeout=3)])
            self.host.proc.stderr.close.assert_called_once_with()
    def test_timeout_kills_then_collects_before_closing_pipes(self):
        self.host.proc.poll.return_value = None
        self.host.proc.wait.side_effect = [CONTROLLER.subprocess.TimeoutExpired("fixture", 3), 0]
        events = mock.Mock()
        events.attach_mock(self.host.proc.wait, "wait")
        events.attach_mock(self.host.proc.stderr.close, "close")
        with mock.patch.object(CONTROLLER.os, "killpg", create=True) as signal:
            events.attach_mock(signal, "signal")
            self.host.close()
            self.assertEqual(events.mock_calls, [
                mock.call.signal(self.host.proc.pid, CONTROLLER.signal.SIGTERM),
                mock.call.wait(timeout=3),
                mock.call.signal(self.host.proc.pid, CONTROLLER.signal.SIGKILL),
                mock.call.wait(), mock.call.close()])

    def test_t3_real_group_child_status_is_collected(self):
        import subprocess
        import sys
        proc = subprocess.Popen([sys.executable, "-u", "-c",
                                 "import signal,sys; signal.signal(signal.SIGTERM, lambda *_: sys.exit(0)); print('ready',flush=True); sys.stdin.read()"],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                start_new_session=True)
        self.host.proc = proc
        try:
            self.assertEqual(proc.stdout.readline(), b"ready\n")
            self.host.close()
            self.assertEqual(proc.returncode, 0)
            self.assertEqual(proc.wait(), 0)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait()
            proc.stdin.close()
            proc.stdout.close()
def host_observer_fixture():
    """Linux Host with retained pidfd collection/exit observers."""
    import subprocess
    import sys
    popen = subprocess.Popen
    guest = r'''
import json,runpy,subprocess,sys,time,threading
from pathlib import Path
ns=runpy.run_path(sys.argv[1])
host=ns['Host']
start,close,logged=host.start,host.close,host.logged
emit=start.__globals__['emit']
audit=Path(sys.argv[1]).resolve().parents[5]/'.cache'/sys.argv[2]/'guest.json'
ready=[]
def capture(value):
    if value['event']=='host.ready': ready.append(value)
    else: emit(value)
start.__globals__['emit']=capture
observer = r"""
import json,os,select,signal,sys
from pathlib import Path
fd=os.pidfd_open(int(sys.argv[1]))
print('observer.ready',flush=True)
try:
    sys.stdin.buffer.read()
    survivors=int(not bool(select.select([fd],[],[],0)[0]))
    if survivors:
        signal.pidfd_send_signal(fd,signal.SIGTERM)
    cleaned=bool(select.select([fd],[],[],4)[0])
    if not cleaned:
        signal.pidfd_send_signal(fd,signal.SIGKILL)
        cleaned=bool(select.select([fd],[],[],4)[0])
    Path(sys.argv[2]+'.exit').write_text(json.dumps({'owned_survivors':survivors,'fixture_cleaned':cleaned}))
finally:
    os.close(fd)
"""
def observed_start(self):
    self.observer=None
    self.observer_reader=None
    deadline=time.monotonic()+20
    stage='guest.readiness'
    try:
        start(self)
        stage='guest.observer'
        self.observer=subprocess.Popen([sys.executable,'-u','-c',observer,str(self.proc.pid),str(audit)],
            stdin=subprocess.PIPE,stdout=subprocess.PIPE,start_new_session=True)
        result=[]
        def read():
            value=self.observer.stdout.readline()
            with self.condition:
                result.append(value)
                self.condition.notify_all()
        self.observer_reader=threading.Thread(target=read,daemon=True)
        self.observer_reader.start()
        with self.condition:
            self.condition.wait_for(lambda: result or self.cancelled.is_set(),max(0,deadline-time.monotonic()))
            if self.cancelled.is_set(): raise RuntimeError('cancelled')
            if result!=[b'observer.ready\n']: raise RuntimeError('observer unavailable')
        emit(ready.pop())
        emit({'event':'guest.owned','pid':self.proc.pid})
    except (OSError,RuntimeError):
        if self.cancelled.is_set(): stage='guest.cancelled'
        emit({'event':'fixture.failed','stage':stage})
        raise RuntimeError('fixture setup failed') from None
def observed_logged(self,marker):
    with self.condition:
        if marker==b'satellite: running':
            print(json.dumps({'event':'guest.command.entered'}),flush=True)
        return logged(self,marker)
def observed_close(self):
    try:
        close(self)
    finally:
        if self.observer is not None:
            self.observer.stdin.close()
            try: self.observer.wait(timeout=1)
            except subprocess.TimeoutExpired:
                self.observer.kill()
                self.observer.wait()
                raise RuntimeError('fixture observer cleanup failed') from None
            finally:
                if self.observer_reader and self.observer_reader.ident is not None:
                    self.observer_reader.join(timeout=1)
                    if self.observer_reader.is_alive(): raise RuntimeError('fixture reader cleanup failed')
                self.observer.stdout.close()
            if self.observer.returncode!=0: raise RuntimeError('fixture observer exited')
    if self.proc is not None:
        audit.write_text(json.dumps({'pid':self.proc.pid,'status':self.proc.returncode,'closed':self.closed}))
host.start,host.close,host.logged=observed_start,observed_close,observed_logged
if __name__=='__main__': sys.exit(ns['main']())
'''
    return guest
