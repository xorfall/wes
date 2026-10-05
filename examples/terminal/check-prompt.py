#!/usr/bin/env python3
"""Synthetic Git prompt acceptance for the actual owned zsh/bash bootstrap files."""
import argparse
import base64
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import selectors
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
ANSI = re.compile(rb'\x1b\[[0-?]*[ -/]*[@-~]')


def quoted(path):
    # Encode test command arguments, including hostile controls, without sending raw escape keys.
    return "$'" + ''.join('\\%03o' % byte for byte in os.fsencode(path)) + "'"


def plain(data):
    return ANSI.sub(b'', data).decode('utf-8', 'strict').replace('\r', '')


class Shell:
    def __init__(self, shell, home, start, git=None, history=None):
        private = home / ('private-' + shell.name + ('-missing' if git == '' else ''))
        private.mkdir(exist_ok=True)
        env = {'HOME': str(home), 'USER': 'fixture', 'LOGNAME': 'fixture', 'PATH': os.environ['PATH'],
               'TERM': 'xterm-256color', 'LANG': 'en_US.UTF-8' if os.uname().sysname == 'Darwin' else 'C.UTF-8',
               'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null',
               'WES_PROMPT_GIT': shutil.which('git') if git is None else git}
        if history is not None: env['WES_HISTORY_FILE'] = str(history)
        self.environment = private / 'environment'
        self.set_environment('DEV')
        env['WES_PROMPT_ENVIRONMENT'] = str(self.environment)
        if shell.name == 'zsh':
            (private / '.zshenv').write_text('unsetopt GLOBAL_RCS\n')
            shutil.copyfile(ROOT / 'crates/app/src/terminal/prompt.zsh', private / '.zshrc')
            env['ZDOTDIR'] = str(private)
            args = [str(shell), '-d', '-i']
        else:
            args = [str(shell), '--noprofile', '--rcfile', str(ROOT / 'crates/app/src/terminal/prompt.bash'), '-i']
        self.pid, self.master = pty.fork()
        if self.pid == 0:
            os.chdir(start)
            os.execve(shell, args, env)
        fcntl.ioctl(self.master, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 240, 0, 0))

    def set_environment(self, name):
        pending = self.environment.with_suffix('.next')
        pending.write_text(name + '\n')
        pending.chmod(0o600)
        pending.replace(self.environment)

    def read(self):
        data = b''
        end = time.monotonic() + 5
        while time.monotonic() < end:
            if select.select([self.master], [], [], .15)[0]:
                data += os.read(self.master, 65536)
            elif data:
                return data
        raise AssertionError('shell prompt timed out: ' + repr(data))

    def command(self, source):
        os.write(self.master, source.encode() + b'\r')
        return self.read()

    def close(self):
        os.close(self.master)
        try: os.kill(self.pid, signal.SIGKILL)
        except ProcessLookupError: pass
        os.waitpid(self.pid, 0)


def fixture(root):
    home = root / 'home'; home.mkdir()
    for name in ['.zshenv', '.zprofile', '.zshrc', '.zlogin', '.bash_profile', '.bashrc', '.profile']:
        (home / name).write_text('touch ' + str(root / 'startup-leaked') + '\n')
    repo = root / 'repo'; repo.mkdir()
    env = {**os.environ, 'HOME': str(home), 'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null'}
    def git(*args):
        return subprocess.check_output(['git', '-c', 'core.hooksPath=/dev/null', '-c', 'core.fsmonitor=false', '-C', str(repo), *args], env=env, stderr=subprocess.PIPE).decode().strip()
    git('init', '--quiet', '--initial-branch=main')
    git('config', 'user.name', 'Synthetic'); git('config', 'user.email', 'synthetic@example.invalid')
    (repo / 'sub').mkdir()
    marker = root / 'external-command-ran'
    hostile_command = 'touch ' + str(marker)
    for key in ['core.fsmonitor', 'core.pager', 'core.sshCommand', 'diff.external', 'alias.symbolic-ref']:
        git('config', key, hostile_command)
    return home, repo, git, marker


def check_shell(shell):
    with tempfile.TemporaryDirectory(prefix='wes-git-prompt-') as directory:
        root = Path(directory); home, repo, git, marker = fixture(root)
        terminal = Shell(shell, home, repo)
        try:
            start = terminal.read()
            assert b'\x1b[32m' in start and b'\x1b[36m' in start and b'\x1b[33m' in start, repr(start)
            assert '(main)' in plain(start) and 'fixture@DEV ' in plain(start), repr(start)
            assert b'\x1b[0m % ' in start and b'\x1b[7m%' not in start, repr(start)
            assert not (root / 'startup-leaked').exists()
            environment = 'Üretim%F{red}$(touch ENV_INJECTED)`touch ENV_BACKTICK`\\e[31m\x1b[31m'
            terminal.set_environment(environment)
            changed = terminal.command(':')
            assert 'fixture@' + environment.replace('\x1b', '?') + ' ' in plain(changed), repr(changed)
            assert b'\x1b[31m' not in changed
            assert not (repo / 'ENV_INJECTED').exists() and not (repo / 'ENV_BACKTICK').exists()
            terminal.set_environment('no-env')
            assert 'fixture@no-env ' in plain(terminal.command(':'))
            terminal.environment.unlink()
            assert 'fixture@no-env ' in plain(terminal.command(':'))
            terminal.set_environment('DEV')
            assert '(main)'  in plain(terminal.command('cd ' + quoted(repo / 'sub')))
            git('-c', 'core.fsmonitor=false', 'commit', '--quiet', '--allow-empty', '-m', 'initial')
            commit = git('rev-parse', '--short', 'HEAD')
            assert '(main)' in plain(terminal.command(':'))
            git('branch', 'feature')
            assert '(feature)' in plain(terminal.command('git -c core.hooksPath=/dev/null -c core.fsmonitor=false checkout --quiet feature'))
            assert '(@' + commit + ')' in plain(terminal.command('git -c core.hooksPath=/dev/null -c core.fsmonitor=false checkout --quiet --detach'))
            linked = root / 'linked'
            git('worktree', 'add', '--quiet', '-b', 'worktree-branch', str(linked), 'main')
            assert '(worktree-branch)' in plain(terminal.command('cd ' + quoted(linked)))
            branch = 'feature%F{red}$(touch${IFS}PROMPT_INJECTED)'
            git('branch', branch, 'main')
            terminal.command('cd ' + quoted(repo))
            output = terminal.command('git -c core.hooksPath=/dev/null -c core.fsmonitor=false checkout --quiet ' + quoted(branch))
            assert '(' + branch + ')' in plain(output), repr(output)
            assert not (repo / 'PROMPT_INJECTED').exists()
            hostile = root / 'ĞğİıŞşÇçÖöÜü%F{red}$(touch PWNED)`touch BACKTICK`\\e[31m\x1b[31m\n'
            hostile.mkdir()
            output = terminal.command('cd ' + quoted(hostile))
            assert 'ĞğİıŞşÇçÖöÜü%F{red}$(touch PWNED)`touch BACKTICK`\\e[31m?[31m?' in plain(output), repr(output)
            assert b'\x1b[31m' not in output
            assert not (hostile / 'PWNED').exists() and not (hostile / 'BACKTICK').exists()
            assert '(worktree-branch)' not in plain(output) and '(' + branch + ')' not in plain(output)
            terminal.command('false')
            assert 'STATUS:1:END' in plain(terminal.command("printf 'STATUS:%s:END\\n' \"$?\""))
            terminal.command('cd ' + quoted(repo))
            assert 'HISTORY:unset:END' in plain(terminal.command("printf 'HISTORY:%s:END\\n' \"${HISTFILE-unset}\""))
            if shell.name == 'zsh':
                assert 'DOTDIR:unset:END' in plain(terminal.command("printf 'DOTDIR:%s:END\\n' \"${ZDOTDIR-unset}\""))
            assert not marker.exists(), 'a Git-configured external command ran'
            assert not (root / 'startup-leaked').exists()
            assert not (home / '.bash_history').exists() and not (home / '.zsh_history').exists()
            print('PASS ' + shell.name + ': ANSI/reset, unborn/subdir/checkout/detached/worktree/no-Git, status, environment changes/no-env, hostile text and startup isolation')
        finally: terminal.close()
        missing = Shell(shell, home, repo, git='')
        try:
            output = missing.read()
            assert b'\x1b[33m' not in output and b'not found' not in output, repr(output)
        finally: missing.close()


def check_application(binary):
    with tempfile.TemporaryDirectory(prefix='wes-git-prompt-app-') as directory:
        root = Path(directory); home, repo, _, marker = fixture(root)
        with (root / 'stderr').open('w+') as errors:
            process = subprocess.Popen([str(binary), '--home', str(root / 'data'), '--serve', '0', '--no-auto-keep'],
                cwd=repo, env={'HOME': str(home), 'USER': 'fixture', 'LOGNAME': 'fixture', 'PATH': os.environ['PATH']},
                stdout=subprocess.PIPE, stderr=errors, text=True)
            events = None
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(process.stdout, selectors.EVENT_READ)
                    assert selector.select(20)
                url = process.stdout.readline().strip().removeprefix('Listening at ')
                events = urllib.request.urlopen(url + '/events', timeout=10)
                generation = None
                while not generation:
                    line = events.readline().decode()
                    if line.startswith('data:'):
                        event = json.loads(line[5:])
                        if event['event'] == 'session': generation = event['generation']
                def post(path, body):
                    req = urllib.request.Request(url + path, json.dumps(body).encode(),
                        {'Content-Type': 'application/json', 'X-Wes-Session': generation})
                    try:
                        with urllib.request.urlopen(req, timeout=10) as response: return json.load(response)
                    except urllib.error.HTTPError as error:
                        raise AssertionError(f'{path}: {error.code}: {error.read().decode()}') from error
                def call(action, client='prompt-fixture', **extra):
                    return post('/terminals', dict(action=action, client=client, **extra))
                def source(text, client='prompt-fixture', context=None):
                    import uuid
                    return post('/submit', dict(request='submit', cell=str(uuid.uuid4()), text=text, client=client, environments=context, console=False))
                def until(predicate):
                    while True:
                        line = events.readline().decode()
                        assert line, 'event stream ended'
                        if not line.startswith('data:'): continue
                        event = json.loads(line[5:])
                        if event['event'] == 'reported':
                            assert not any(d.get('severity') == 'error' for d in event.get('diagnostics', [])), event
                        if predicate(event): return event
                cursors = {}
                def output(identity, client='prompt-fixture'):
                    data = b''
                    deadline = time.monotonic() + 5
                    while time.monotonic() < deadline:
                        frame = call('poll', client=client, id=identity, cursor=cursors.get(identity, 0), wait_ms=150)
                        chunk = base64.b64decode(frame['data']); cursors[identity] = frame['next']; data += chunk
                        if data and not chunk: return data
                    raise AssertionError('native prompt did not arrive: ' + repr(data))
                def await_label(identity, expected, client='prompt-fixture'):
                    deadline = time.monotonic() + 5
                    while time.monotonic() < deadline:
                        call('write', client=client, id=identity, text=':\r')
                        data = output(identity, client)
                        if 'fixture@' + expected + ' ' in plain(data): return
                    raise AssertionError('native environment label not updated: ' + repr(data))
                identity = call('start')['id']
                data = output(identity)
                assert '(main)' in plain(data) and b'\x1b[33m' in data, repr(data)
                assert 'fixture@default ' in plain(data), repr(data)
                # One client overrides the default; another terminal still uses the default.
                recipe = {'version': 1, 'package': 'prompt-environments', 'environments': {'DEV': {}, 'PROD': {}}}
                (repo / 'prompt-environments.yaml').write_text(json.dumps(recipe))
                source(':env plan file:"prompt-environments.yaml" > promptPlan')
                source(':env apply $promptPlan')
                environment = until(lambda event: event['event'] == 'environments' and 'DEV' in event['revisions'])
                def choose(name):
                    context = {'selected': name, 'revisions': environment['revisions']}
                    source(':env clear' if name is None else ':env use ' + json.dumps(name, ensure_ascii=False), context=context)
                    until(lambda event: event['event'] == 'environments' and event.get('clients', {}).get('prompt-fixture', {}).get('selected', 'absent') == name)
                choose('DEV')
                fresh = call('start')['id']
                assert 'fixture@DEV ' in plain(output(fresh))  # Already correct on the very first prompt.
                untouched = call('start', client='other-fixture')['id']
                assert 'fixture@default ' in plain(output(untouched, 'other-fixture'))
                await_label(identity, 'DEV')
                choose('PROD'); await_label(identity, 'PROD'); await_label(fresh, 'PROD')
                choose(None); await_label(identity, 'no-env'); await_label(fresh, 'no-env')
                await_label(untouched, 'default', 'other-fixture')
                assert not marker.exists() and not (root / 'startup-leaked').exists()
                for terminal in [identity, fresh]: call('close', id=terminal)
                call('close', client='other-fixture', id=untouched)
                print('PASS native terminal launch: first prompt/default/client override/live environment/no-env and startup isolation')
            finally:
                if events: events.close()
                process.send_signal(signal.SIGINT)
                try: process.wait(timeout=15)
                except subprocess.TimeoutExpired: process.kill(); process.wait()
                if process.returncode != 0:
                    errors.seek(0); raise AssertionError(errors.read())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/wes')
    args = parser.parse_args()
    for shell in [Path('/bin/zsh'), Path('/bin/bash')]:
        if shell.exists(): check_shell(shell)
    check_application(args.binary.resolve())


if __name__ == '__main__': main()
