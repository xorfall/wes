#!/usr/bin/env python3
"""Private ↑ history, real shells and native server, only isolated synthetic homes."""
import argparse
import base64
import importlib.util
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('prompt_fixture', Path(__file__).with_name('check-prompt.py'))
prompt = importlib.util.module_from_spec(spec); spec.loader.exec_module(prompt)


def wait_for(predicate):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if predicate(): return
        time.sleep(.02)
    raise AssertionError('history write did not arrive')


def check_shell(shell):
    with tempfile.TemporaryDirectory(prefix='wes-shell-history-') as directory:
        root = Path(directory); home = root / 'user'; home.mkdir()
        history = root / 'private-history'; history.touch(mode=0o600)
        marker = root / 'executed'
        source = "printf '%s\\n' 'ĞğİıŞşÇçÖöÜü' >> " + prompt.quoted(marker)
        terminal = prompt.Shell(shell, home, root, git='', history=history)
        try:
            terminal.read()
            terminal.command('false')
            assert 'STATUS:1:END' in prompt.plain(terminal.command("printf 'STATUS:%s:END\\n' \"$?\""))
            terminal.command(source)
            wait_for(lambda: marker.exists())
            assert marker.read_text() == 'ĞğİıŞşÇçÖöÜü\n'
            os.write(terminal.master, b'NOT_SUBMITTED')
            terminal.read()
        finally: terminal.close()
        terminal = prompt.Shell(shell, home, root, git='', history=history)
        try:
            terminal.read()
            assert marker.read_text() == 'ĞğİıŞşÇçÖöÜü\n', 'restoring history executed a command'
            os.write(terminal.master, b'\x1b[A')
            recalled = prompt.plain(terminal.read())
            assert 'ĞğİıŞşÇçÖöÜü' in recalled and 'printf' in recalled, repr(recalled)
            assert b'NOT_SUBMITTED' not in history.read_bytes()
            os.write(terminal.master, b'\x03'); terminal.read()
            # A literal multiline compound command is one recalled entry.
            terminal.command("{ printf 'MULTI_';\nprintf 'ÜNICODE\\n'; }")
        finally: terminal.close()
        terminal = prompt.Shell(shell, home, root, git='', history=history)
        try:
            terminal.read()
            os.write(terminal.master, b'\x1b[A')
            recalled = prompt.plain(terminal.read())
            # Bash's ↑ walks physical lines in a recalled multiline entry; inspect its
            # complete last native entry too, without executing the recalled command.
            os.write(terminal.master, b'\x03'); terminal.read()
            last = prompt.plain(terminal.command('fc -ln -1'))
            assert 'MULTI_' in last and 'ÜNICODE' in last, repr((recalled, last))
            os.write(terminal.master, b'sleep 30; : LAST_RUNNING_COMMAND\r')
            terminal.read()  # Drain macOS PTY output so zsh can finish its tty transition.
            wait_for(lambda: b'LAST_RUNNING_COMMAND' in history.read_bytes())
        finally: terminal.close()
        terminal = prompt.Shell(shell, home, root, git='', history=history)
        try:
            terminal.read()
            os.write(terminal.master, b'\x1b[A')
            assert 'LAST_RUNNING_COMMAND' in prompt.plain(terminal.read())
        finally: terminal.close()
        seeds = [(': SEED_%04d' % i).encode() for i in range(1000)]
        history.write_bytes(b'\0'.join(seeds) + b'\0')
        terminal = prompt.Shell(shell, home, root, git='', history=history)
        try:
            terminal.read()
            terminal.command(': BOUNDED_LATEST')
        finally: terminal.close()
        records = history.read_bytes().split(b'\0')[:-1]
        assert len(records) == 1000 and records == seeds[1:] + [b': BOUNDED_LATEST'], (len(records), records[:2], records[-2:])
        assert history.stat().st_mode & 0o777 == 0o600
        assert not (home / '.bash_history').exists() and not (home / '.zsh_history').exists()
        assert b'_wes_' not in history.read_bytes(), 'bootstrap helpers became user history'
        print('PASS', shell.name, '↑ fresh-process, Unicode/multiline, status, running command, drafts excluded, no replay/global history')


class Server:
    def __init__(self, binary, root, data):
        self.errors = (root / 'stderr').open('w+')
        self.process = subprocess.Popen([str(binary), '--home', str(data), '--serve', '0', '--no-auto-keep'],
            cwd=root, env={'HOME': str(root / 'user'), 'USER': 'fixture', 'LOGNAME': 'fixture', 'PATH': os.environ['PATH']},
            stdout=subprocess.PIPE, stderr=self.errors, text=True)
        with selectors.DefaultSelector() as selector:
            selector.register(self.process.stdout, selectors.EVENT_READ)
            assert selector.select(20)
        self.url = self.process.stdout.readline().strip().removeprefix('Listening at ')
        if not self.url:
            self.errors.flush(); self.errors.seek(0)
            raise AssertionError('native server exited before listening: status=%r stderr=%s' % (self.process.poll(), self.errors.read()))
        self.events = urllib.request.urlopen(self.url + '/events', timeout=10)
        self.generation = None
        while not self.generation:
            line = self.events.readline().decode()
            if line.startswith('data:'):
                event = json.loads(line[5:])
                if event['event'] == 'session': self.generation = event['generation']
        self.cursors = {}

    def call(self, action, client='history-fixture', **extra):
        request = urllib.request.Request(self.url + '/terminals', json.dumps(dict(action=action, client=client, **extra)).encode(),
            {'Content-Type': 'application/json', 'X-Wes-Session': self.generation})
        with urllib.request.urlopen(request, timeout=10) as response: return json.load(response)

    def output(self, identity):
        data = b''; deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            frame = self.call('poll', id=identity, cursor=self.cursors.get(identity, 0), wait_ms=150)
            chunk = base64.b64decode(frame['data']); self.cursors[identity] = frame['next']; data += chunk
            if data and not chunk: return prompt.plain(data)
        raise AssertionError('no terminal output: ' + repr(data))

    def start(self, key):
        identity = self.call('start', history=key)['id']; self.output(identity); return identity

    def recall(self, identity):
        self.call('write', id=identity, text='\x1b[A'); return self.output(identity)

    def close(self):
        self.events.close()
        self.process.send_signal(signal.SIGINT)
        try: self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill(); self.process.wait(); raise
        finally: self.errors.close()


def check_application(binary):
    with tempfile.TemporaryDirectory(prefix='wes-native-history-') as directory:
        root = Path(directory); (root / 'user').mkdir()
        data = root / 'data'; key = str(uuid.uuid4()); other = str(uuid.uuid4())
        source = "printf 'ONLY_ONCE\\n' >> marker # NATIVE_ÜNICODE"
        server = Server(binary, root, data)
        try:
            terminal = server.start(key)
            server.call('write', id=terminal, text=source + '\r'); server.output(terminal)
            second = server.start(other)
            server.call('write', id=second, text=': SECOND_PANE\r'); server.output(second)
            # Generic end is not explicit pane deletion; restart must preserve ↑.
            server.call('close', id=terminal)
            terminal = server.start(key)
            assert 'NATIVE_ÜNICODE' in server.recall(terminal)
            server.call('write', id=terminal, text='\x03'); server.output(terminal)
            server.call('write', id=terminal, text='sleep 30; : RUNNING_ON_QUIT\r')
            wait_for(lambda: any(b'RUNNING_ON_QUIT' in f.read_bytes() for f in (data/'terminal-history'/key).iterdir()))
        finally: server.close()
        assert (root/'marker').read_text() == 'ONLY_ONCE\n'
        server = Server(binary, root, data)
        try:
            terminal = server.start(key)
            assert 'RUNNING_ON_QUIT' in server.recall(terminal)
            second = server.start(other)
            assert 'SECOND_PANE' in server.recall(second)
            assert (root/'marker').read_text() == 'ONLY_ONCE\n'
            server.call('forget', history=key)
            assert not (data/'terminal-history'/key).exists()
            try: server.call('start', history=key)
            except urllib.error.HTTPError as error: assert error.code == 400
            else: raise AssertionError('late start resurrected forgotten history')
            server.call('forget', history=key)  # Idempotent retry.
            fresh = server.start(str(uuid.uuid4()))
            server.call('write', id=fresh, text="printf 'COUNT:%s:END\\n' \"$(fc -ln -1 2>/dev/null)\"\r")
            assert 'NATIVE_' not in server.output(fresh)
        finally: server.close()
        # Same UUID in a different selected data home starts empty; old home's other
        # pane survives when that home is opened again.
        server = Server(binary, root, root/'other-data')
        try:
            terminal = server.start(other)
            server.call('write', id=terminal, text=': OTHER_HOME\r'); server.output(terminal)
        finally: server.close()
        server = Server(binary, root, data)
        try:
            terminal = server.start(other)
            recalled = server.recall(terminal)
            assert 'SECOND_PANE' in recalled and 'OTHER_HOME' not in recalled
        finally: server.close()
        assert not (root/'user/.bash_history').exists() and not (root/'user/.zsh_history').exists()
        print('PASS native: restart/process isolation, app quit, explicit forget/late start, home isolation, no command replay')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(); parser.add_argument('--binary', type=Path, required=True); args = parser.parse_args()
    for name in ['bash', 'zsh']:
        shell = Path('/bin') / name
        if shell.exists(): check_shell(shell)
    check_application(args.binary.resolve())
