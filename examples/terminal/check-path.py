#!/usr/bin/env python3
"""Desktop PATH acceptance with fake local executables, no accounts or model requests."""
import argparse
import base64
import json
from pathlib import Path
import selectors
import shlex
import signal
import subprocess
import tempfile
import time
import urllib.request


def check(binary, mode):
    with tempfile.TemporaryDirectory(prefix='wes-terminal-path-') as temporary:
        root = Path(temporary)
        home = root / 'Üser Home'
        local = home / '.local/bin'
        local.mkdir(parents=True)
        preferred = root / 'preferred'
        preferred.mkdir()
        # Require the launcher's three transport overrides, then only --version.
        # Discovery, inherited PATH precedence and TTY checks remain unchanged.
        for directory, label in [(local, 'local'), (preferred, 'preferred')]:
            executable = directory / 'codex'
            executable.write_text('#!/bin/sh\n'
                'test "$#" = 7 && test "$1" = -c && test "$3" = -c && test "$5" = -c || exit 40\n'
                'case "$2" in mcp_servers.wes_workspace.command=*) ;; *) exit 40 ;; esac\n'
                'test "$4" = \'mcp_servers.wes_workspace.args=[]\' || exit 40\n'
                'test "$6" = \'mcp_servers.wes_workspace.env_vars=["WES_MCP_METRICS_DIR"]\' || exit 40\n'
                'shift 6\n'
                'test "$#" = 1 && test "$1" = --version || exit 41\n'
                'test -t 0 && test -t 1 || exit 42\n'
                'test -z "${WES_SYNTHETIC_CREDENTIAL+x}" || exit 43\n'
                f'printf "SYNTHETIC_CODEX_{label}\\n"\n')
            executable.chmod(0o700)
        for name in ['.zshenv', '.zshrc', '.zprofile', '.bashrc', '.bash_profile', '.profile']:
            (home / name).write_text('printf BAD_STARTUP > "$HOME/startup-read"\n')
        env = {'HOME': str(home), 'USER': 'fixture', 'LOGNAME': 'fixture',
               'TMPDIR': str(root), 'WES_SYNTHETIC_CREDENTIAL': 'synthetic-only'}
        if mode == 'desktop': env['PATH'] = '/usr/bin:/bin'
        elif mode == 'preferred': env['PATH'] = str(preferred) + ':/usr/bin:/bin'
        elif mode == 'empty': env['PATH'] = ''
        expected = 'preferred' if mode == 'preferred' else 'local'
        with (root / 'stderr').open('w+') as errors:
            process = subprocess.Popen([str(binary), '--home', str(root / 'data'),
                '--serve', '0', '--no-auto-keep'], cwd=root, env=env,
                stdout=subprocess.PIPE, stderr=errors, text=True)
            events = None
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(process.stdout, selectors.EVENT_READ)
                    assert selector.select(20), 'server startup timed out'
                url = process.stdout.readline().strip().removeprefix('Listening at ')
                events = urllib.request.urlopen(url + '/events', timeout=10)
                generation = None
                while not generation:
                    line = events.readline().decode()
                    assert line, 'event stream ended'
                    if line.startswith('data:'):
                        event = json.loads(line[5:])
                        if event['event'] == 'session': generation = event['generation']

                def call(action, **values):
                    request = urllib.request.Request(url + '/terminals',
                        json.dumps(dict(client='path-fixture', action=action, **values)).encode(),
                        {'Content-Type': 'application/json', 'X-Wes-Session': generation})
                    with urllib.request.urlopen(request, timeout=10) as response:
                        return json.load(response)

                identity = call('start')['id']
                cursor = 0

                def until(marker):
                    nonlocal cursor
                    output = b''
                    deadline = time.monotonic() + 10
                    while time.monotonic() < deadline:
                        frame = call('poll', id=identity, cursor=cursor, wait_ms=150)
                        assert frame['start'] == cursor and not frame['problem'], frame
                        chunk = base64.b64decode(frame['data'])
                        assert frame['next'] == cursor + len(chunk)
                        cursor = frame['next']
                        output += chunk
                        if marker in output: return output
                    raise AssertionError(repr(output))

                # Build the marker from printf arguments so terminal echo cannot satisfy it.
                call('write', id=identity, text="printf 'READY:%s\\n' terminal\r")
                until(b'READY:terminal')
                for command in ['codex --version', "/bin/sh -c 'codex --version'"]:
                    call('write', id=identity,
                        text=command + "; printf 'DONE:%s:%s\\n' terminal $?\r")
                    output = until(b'DONE:terminal:')
                    assert f'SYNTHETIC_CODEX_{expected}'.encode() in output, repr(output)
                    assert b'DONE:terminal:0' in output, repr(output)
                # PATH is recorded as data; spaces/Unicode must remain one intact directory.
                result = root / 'path-result'
                call('write', id=identity, text='printf "%s" "$PATH" > ' +
                    shlex.quote(str(result)) + "; printf 'SAVED:%s\\n' terminal\r")
                until(b'SAVED:terminal')
                entries = result.read_text().split(':')
                assert entries[0].endswith('/assistants'), entries
                assert entries[-1] + '/assistants' == entries[0], entries
                assert str(local) in entries, entries
                assert '' not in entries, entries
                if mode == 'preferred': assert entries.index(str(preferred)) < entries.index(str(local))
                assert not (home / 'startup-read').exists(), 'user shell startup was sourced'
                call('close', id=identity)
                print(f'PASS PATH={mode}: local executable, native precedence, child shell, TTY, startup/credential isolation')
            finally:
                if events: events.close()
                process.send_signal(signal.SIGINT)
                try: process.wait(timeout=15)
                except subprocess.TimeoutExpired: process.kill(); process.wait()
                if process.returncode != 0:
                    errors.seek(0)
                    raise AssertionError(errors.read())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path,
        default=Path(__file__).resolve().parents[2] / 'target/debug/wes')
    binary = parser.parse_args().binary.resolve()
    for mode in ['desktop', 'preferred', 'missing', 'empty']:
        check(binary, mode)


if __name__ == '__main__': main()
