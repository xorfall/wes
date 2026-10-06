"""An example's program, deadline-bound output and orderly shutdown.

The owner reaps its direct child and joins its capture worker on every path. An inherited
output writer must not block capture teardown; missing EOF is reported instead of passing
as complete output. Local invocation descendants are managed by the application's executor.
"""
import ctypes
import os
from pathlib import Path
import queue
import selectors
import signal
import subprocess
import threading

_WINDOWS = os.name == 'nt'
_LINE_BYTES = 64 * 1024


class Owned:
    def __init__(self, arguments, *, cwd, log, env=None, patience=15):
        self.forced = False
        self._patience = patience
        self._lines = queue.Queue(maxsize=16)
        self._stop_reading = threading.Event()
        self._output_ended = False
        self._closed = False
        self._capture_error = None
        self._log_path = Path(log)
        self._diagnostics = ''
        self._log = self._log_path.open('wb')
        self._reader = None
        self.process = None
        try:
            self.process = subprocess.Popen(
                [str(argument) for argument in arguments], cwd=cwd, env=env,
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=self._log,
                # Raw bytes: teardown never contends with a buffered readline lock.
                bufsize=0,
                creationflags=subprocess.CREATE_NEW_PROCESS_GROUP if _WINDOWS else 0)
            self._reader = threading.Thread(target=self._read)
            self._reader.start()
        except BaseException:
            self._release()
            raise

    def _enqueue(self, value):
        while not self._stop_reading.is_set():
            try:
                self._lines.put(value, timeout=.05)
                return
            except queue.Full:
                pass

    def _read(self):
        pending = bytearray()
        selector = None
        try:
            fd = self.process.stdout.fileno()
            if _WINDOWS:
                import msvcrt
                from ctypes import wintypes
                handle = msvcrt.get_osfhandle(fd)
                peek = ctypes.WinDLL('kernel32', use_last_error=True).PeekNamedPipe
                peek.argtypes = [wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD,
                                 ctypes.c_void_p, ctypes.POINTER(wintypes.DWORD), ctypes.c_void_p]
                peek.restype = wintypes.BOOL
            else:
                selector = selectors.DefaultSelector()
                selector.register(fd, selectors.EVENT_READ)
                os.set_blocking(fd, False)
            while not self._stop_reading.is_set():
                if _WINDOWS:
                    available = wintypes.DWORD()
                    if not peek(handle, None, 0, None, ctypes.byref(available), None):
                        error = ctypes.get_last_error()
                        if error == 109:  # ERROR_BROKEN_PIPE: every writer closed.
                            break
                        raise ctypes.WinError(error)
                    if not available.value:
                        self._stop_reading.wait(.05)
                        continue
                    count = min(available.value, 8192)
                else:
                    if not selector.select(.05):
                        continue
                    count = 8192
                try:
                    data = os.read(fd, count)
                except BlockingIOError:
                    continue
                if not data:
                    break
                pending.extend(data)
                while b'\n' in pending:
                    line, _, rest = pending.partition(b'\n')
                    if len(line) > _LINE_BYTES:
                        raise RuntimeError('program output line exceeds capture budget')
                    pending = bytearray(rest)
                    self._enqueue(line.decode('utf-8', errors='replace').rstrip('\r'))
                if len(pending) > _LINE_BYTES:
                    raise RuntimeError('program output line exceeds capture budget')
            if not self._stop_reading.is_set():
                self._output_ended = True
                if pending:
                    self._enqueue(pending.decode('utf-8', errors='replace').rstrip('\r'))
                self._enqueue(None)
        except BaseException as error:
            self._capture_error = f'program output capture failed: {error}'
            self._enqueue(error)
        finally:
            if selector is not None:
                selector.close()

    def line(self, timeout):
        if self._closed:
            raise RuntimeError(f'program output is closed\n{self.log()}')
        try:
            line = self._lines.get(timeout=timeout)
        except queue.Empty:
            raise RuntimeError(f'no output within {timeout} seconds\n{self.log()}') from None
        if isinstance(line, BaseException):
            raise RuntimeError(f'program output could not be captured: {line}\n{self.log()}')
        if line is None:
            self._enqueue(None)
            raise RuntimeError(f'the program ended without the expected output\n{self.log()}')
        return line

    def log(self):
        if self._closed:
            return self._diagnostics
        # A separate file description: reading never seeks the child's stderr writer.
        self._log.flush()
        return self._log_path.read_bytes().decode('utf-8', errors='replace')

    def running(self):
        return self.process is not None and self.process.poll() is None

    def stop(self):
        if self._closed:
            if self._capture_error:
                raise RuntimeError(f'{self._capture_error}\n{self.log()}')
            return None if self.process is None else self.process.returncode
        if self.process is not None and self.process.poll() is None:
            try:
                self.process.send_signal(signal.CTRL_BREAK_EVENT if _WINDOWS else signal.SIGINT)
                self.process.wait(timeout=self._patience)
            except (OSError, ValueError, subprocess.TimeoutExpired):
                self.forced = True
        return self._release()

    def finish(self):
        """Stop and require orderly success, including complete output capture."""
        code = self.stop()
        if self.forced:
            raise RuntimeError(f'the program did not stop when asked and was ended by force\n{self.log()}')
        if code != 0:
            raise RuntimeError(f'the program exited with code {code}\n{self.log()}')
        return code

    def _release(self):
        if self._closed:
            return None if self.process is None else self.process.returncode
        code = None
        incomplete = False
        try:
            if self.process is not None:
                if self.process.poll() is None:
                    self.forced = True
                    self.process.kill()
                code = self.process.wait(timeout=5)
                if self._reader is not None and self._reader.ident is not None:
                    self._reader.join(timeout=1)
                    incomplete = not self._output_ended
                    self._stop_reading.set()
                    self._reader.join(timeout=2)
                    if self._reader.is_alive():
                        raise RuntimeError('program output capture did not stop')
        finally:
            self._stop_reading.set()
            if self._reader is not None and self._reader.ident is not None:
                self._reader.join(timeout=2)
            if self.process is not None:
                self.process.stdout.close()
            self._diagnostics = self.log()
            self._log.close()
            self._closed = True
        if incomplete:
            if self._capture_error is None:
                self._capture_error = 'program output stayed open after exit; capture was cancelled'
            raise RuntimeError(f'{self._capture_error}\n{self.log()}')
        return code

    def __enter__(self):
        return self

    def __exit__(self, kind, error, trace):
        if kind is None:
            self.finish()
        else:
            self.stop()
        return False
