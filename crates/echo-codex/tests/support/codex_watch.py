"""T3 OS exit observer. Acquire a handle before acknowledging readiness."""
import os
import sys

pid = int(sys.argv[1])
if sys.platform == 'win32':
    import ctypes
    from ctypes import wintypes
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    kernel.WaitForSingleObject.restype = wintypes.DWORD
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    handle = kernel.OpenProcess(0x00100000, False, pid)
    assert handle, ctypes.get_last_error()
    print('WATCHING', flush=True)
    try:
        assert kernel.WaitForSingleObject(handle, 5000) == 0, 'child did not exit'
    finally:
        kernel.CloseHandle(handle)
else:
    import select
    descriptor = os.pidfd_open(pid)
    print('WATCHING', flush=True)
    try:
        assert select.select([descriptor], [], [], 5)[0] == [descriptor], 'child did not exit'
    finally:
        os.close(descriptor)
print('EXITED', flush=True)
