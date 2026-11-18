"""T3 OS exit observer. Acquire a handle before acknowledging readiness."""
import os
import sys

pid = int(sys.argv[1])
import select
descriptor = os.pidfd_open(pid)
print('WATCHING', flush=True)
try:
    assert select.select([descriptor], [], [], 5)[0] == [descriptor], 'child did not exit'
finally:
    os.close(descriptor)
print('EXITED', flush=True)
