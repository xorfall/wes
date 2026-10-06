#!/usr/bin/env python3
"""Check that an example's owned program is stopped and reaped in every outcome."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time

import lifecycle

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, required=True)
parser.add_argument('--without-console', action='store_true', help=argparse.SUPPRESS)
options = parser.parse_args()
binary = options.binary.resolve()
STUBBORN = ('import signal, sys, time\n'
            'for name in ("SIGINT", "SIGBREAK"):\n'
            '    if hasattr(signal, name): signal.signal(getattr(signal, name), signal.SIG_IGN)\n'
            'print("ready", flush=True)\n'
            'time.sleep(600)\n')


def alive(process):
    return process.poll() is None


def stop_writer(pid):
    if os.name != 'nt':
        os.kill(pid, __import__('signal').SIGTERM)
        return
    # TerminateProcess returns before the process releases its working directory. Hold
    # this explicitly owned writer's handle until it exits before deleting the fixture.
    import ctypes
    from ctypes import wintypes
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    opened = kernel.OpenProcess
    opened.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    opened.restype = wintypes.HANDLE
    terminate = kernel.TerminateProcess
    terminate.argtypes = [wintypes.HANDLE, wintypes.UINT]
    terminate.restype = wintypes.BOOL
    wait = kernel.WaitForSingleObject
    wait.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    wait.restype = wintypes.DWORD
    close = kernel.CloseHandle
    close.argtypes = [wintypes.HANDLE]
    close.restype = wintypes.BOOL
    handle = opened(0x100001, False, pid)  # SYNCHRONIZE | PROCESS_TERMINATE
    if not handle:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        if not terminate(handle, 1):
            raise ctypes.WinError(ctypes.get_last_error())
        if wait(handle, 10000) != 0:
            raise RuntimeError('the fixture writer did not finish its termination')
    finally:
        close(handle)


def served(root):
    return lifecycle.Owned([binary, '--home', root / 'home', '--serve', '0'], cwd=root, log=root / 'log')


if options.without_console:
    # Started by the check below with no console at all. A stop request cannot be addressed
    # to the program from here, so it is ended by force, and that is reported, not hidden.
    with tempfile.TemporaryDirectory(prefix='wes-lifecycle-') as directory:
        app = served(Path(directory))
        listening = app.line(45).startswith('Listening at http://')
        code = app.stop()
        print(json.dumps({'listening': listening, 'forced': app.forced, 'ended': not alive(app.process),
                          'code': code}))
    sys.exit(0)

# A server stops when asked, by its own orderly exit, and its directory can then be removed.
with tempfile.TemporaryDirectory(prefix='wes-lifecycle-') as directory:
    root = Path(directory)
    with served(root) as app:
        assert app.line(45).startswith('Listening at http://')
        process = app.process
    assert not alive(process) and process.returncode == 0 and not app.forced, app.forced
    assert (root / 'home' / 'identity.json').is_file()

# The same when the example fails in the middle: the error is the example's, nothing is left.
with tempfile.TemporaryDirectory(prefix='wes-lifecycle-') as directory:
    try:
        with served(Path(directory)) as app:
            app.line(45)
            process = app.process
            raise KeyError('the example failed')
    except KeyError:
        pass
    assert not alive(process) and process.returncode == 0 and not app.forced

# A program that cannot be started leaves no open log behind.
with tempfile.TemporaryDirectory(prefix='wes-lifecycle-') as directory:
    root = Path(directory)
    try:
        lifecycle.Owned([root / 'no-such-program'], cwd=root, log=root / 'log')
        raise AssertionError('a missing program started')
    except OSError:
        pass

# A program that ends before saying anything is reported with what it wrote, and reaped.
with tempfile.TemporaryDirectory(prefix='wes-lifecycle-') as directory:
    root = Path(directory)
    app = lifecycle.Owned([binary, '--no-such-option'], cwd=root, log=root / 'log')
    try:
        app.line(45)
        raise AssertionError('a refused launch produced output')
    except RuntimeError as error:
        assert 'no-such-option' in str(error), error
    assert app.stop() != 0 and not app.forced and not alive(app.process)
    assert 'no-such-option' in app.log()
    assert app.stop() == app.process.returncode

# Silence is bounded.
with tempfile.TemporaryDirectory(prefix='wes-lifecycle-') as directory:
    root = Path(directory)
    silent = ('import signal, sys, time\n'
              'for name in ("SIGINT", "SIGBREAK"):\n'
              '    if hasattr(signal, name): signal.signal(getattr(signal, name), lambda *_: sys.exit(0))\n'
              'while True: time.sleep(.05)\n')
    with lifecycle.Owned([sys.executable, '-c', silent], cwd=root,
                         log=root / 'log') as app:
        try:
            app.line(0.5)
            raise AssertionError('a silent program produced output')
        except RuntimeError as error:
            assert 'no output within' in str(error), error
    assert not alive(app.process)

# A program that ignores the request is ended so nothing is left, and that is a failure.
with tempfile.TemporaryDirectory(prefix='wes-lifecycle-') as directory:
    root = Path(directory)
    try:
        with lifecycle.Owned([sys.executable, '-c', STUBBORN], cwd=root, log=root / 'log',
                             patience=1) as app:
            assert app.line(45) == 'ready'
        raise AssertionError('a forced end passed as an orderly one')
    except RuntimeError as error:
        assert 'ended by force' in str(error), error
    assert app.forced and not alive(app.process)
    assert app.log() == ''

# A descendant holding the pipe cannot trap capture teardown after the direct child exits.
# This synthetic writer is explicitly owned and ended by the fixture, not a user process.
with tempfile.TemporaryDirectory(prefix='wes-lifecycle-') as directory:
    root = Path(directory)
    holder = None
    script = ("import subprocess, sys; child=subprocess.Popen([sys.executable, '-c', "
              "'import time; time.sleep(600)'], stdout=sys.stdout, stderr=subprocess.DEVNULL); "
              "print(child.pid, flush=True)")
    app = lifecycle.Owned([sys.executable, '-c', script], cwd=root, log=root / 'log', patience=.1)
    try:
        holder = int(app.line(45))
        app.process.wait(timeout=10)
        started = time.monotonic()
        try:
            app.stop()
            raise AssertionError('an inherited writer passed as complete capture')
        except RuntimeError as error:
            assert 'output stayed open' in str(error), error
        assert time.monotonic() - started < 4
        assert not app._reader.is_alive() and app.process.stdout.closed
        assert app.log() == ''
    finally:
        if holder is not None:
            stop_writer(holder)

# The monitor uses the same orderly-success check and cleans its event reader if it fails.
spec = importlib.util.spec_from_file_location('monitor_support', Path(__file__).parent / 'live-service-monitor' / 'support.py')
monitor = importlib.util.module_from_spec(spec)
spec.loader.exec_module(monitor)
with tempfile.TemporaryDirectory(prefix='wes-lifecycle-') as directory:
    root = Path(directory)
    client = monitor.Client.__new__(monitor.Client)
    client.owned = lifecycle.Owned([sys.executable, '-c', STUBBORN], cwd=root, log=root / 'log', patience=.1)
    assert client.owned.line(45) == 'ready'
    released = threading.Event()
    class Stream:
        def close(self): released.set()
    client.stream = Stream()
    client.reader = threading.Thread(target=released.wait)
    client.reader.start()
    try:
        client.close()
        raise AssertionError('the monitor accepted a forced stop')
    except RuntimeError as error:
        assert 'ended by force' in str(error), error
    assert released.is_set() and not client.reader.is_alive()
    assert client.owned.forced and not alive(client.owned.process)

# Windows: started without a console, the request has no way to the program. It is still
# ended and reaped, and the helper says that it had to use force.
if os.name == 'nt':
    result = subprocess.run([sys.executable, __file__, '--binary', str(binary), '--without-console'],
                            capture_output=True, text=True, timeout=120,
                            creationflags=subprocess.DETACHED_PROCESS)
    assert result.returncode == 0, result.stdout + result.stderr
    report = json.loads(result.stdout)
    assert report['listening'] and report['ended'], report
    assert report['forced'], report

print('lifecycle checks passed')
