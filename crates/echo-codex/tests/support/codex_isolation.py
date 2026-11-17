"""T3 two live pinned sessions: drop one before allowing the other to finish."""
import pathlib
import queue
import subprocess
import sys
import threading


def read_lines(child, output):
    for line in child.stdout:
        output.put(line)
    output.put(None)


def until(output, marker):
    while True:
        line = output.get(timeout=25)
        assert line is not None, 'probe ended before readiness: ' + marker
        if line.strip() == marker:
            return


def finish(child, output):
    lines = []
    while True:
        line = output.get(timeout=25)
        if line is None:
            break
        lines.append(line)
    assert child.wait(timeout=5) == 0, ''.join(lines) + child.stderr.read()
    assert 'PUBLIC_CLIENT_PASS\n' in lines, lines


def main():
    children = []
    try:
        for case in ('drop', 'survivor'):
            child = subprocess.Popen([sys.executable, str(pathlib.Path(__file__).with_name('codex_probe.py')),
                                      sys.argv[1], '--rust-client', sys.argv[2], '--case', case],
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            output = queue.Queue()
            threading.Thread(target=read_lines, args=(child, output), daemon=True).start()
            children.append((child, output))
        for _, output in children:
            until(output, 'ISOLATION_HELD')
        victim, output = children[0]
        victim.stdin.write('drop\n')
        victim.stdin.flush()
        finish(victim, output)
        survivor, output = children[1]
        assert survivor.poll() is None, 'survivor exited before victim cleanup'
        survivor.stdin.write('resume\n')
        survivor.stdin.flush()
        finish(survivor, output)
        print('DROP_SURVIVOR_PASS held_sessions=2 victim_exited=true victim_sockets=0 survivor_append=1')
    finally:
        for child, _ in children:
            if child.poll() is None:
                child.kill()
            child.wait(timeout=5)


if __name__ == '__main__':
    main()
