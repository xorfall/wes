#!/usr/bin/env python3
"""Real terminal acceptance under desktop-launcher locale settings; synthetic data only."""
import argparse
import base64
import json
import os
from pathlib import Path
import re
import selectors
import signal
import subprocess
import tempfile
import time
import urllib.request

TEXT = 'ĞğİıŞşÇçÖöÜü'
COMPLETED = b'WES_COMMAND_DONE'
# The echoed command cannot contain the marker that printf assembles.
FINISH = "; printf '\\n%s%s\\n' 'WES_COMMAND_' 'DONE'\r"


def check(binary, locale):
    with tempfile.TemporaryDirectory(prefix='wes-terminal-unicode-') as directory:
        root = Path(directory)
        # No user dotfiles, preferences, values, credentials or ambient locale enter this server.
        env = {'HOME': str(root), 'USER': 'fixture', 'LOGNAME': 'fixture',
               'PATH': '/usr/bin:/bin', 'TMPDIR': str(root), **locale}
        with (root / 'stderr').open('w+') as errors:
            process = subprocess.Popen([str(binary), '--home', str(root / 'data'), '--serve', '0', '--no-auto-keep'],
                                       cwd=root, env=env, stdout=subprocess.PIPE, stderr=errors, text=True)
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
                    if line.startswith('data:'):
                        value = json.loads(line[5:])
                        if value['event'] == 'session': generation = value['generation']

                def call(action, **values):
                    request = urllib.request.Request(url + '/terminals',
                        json.dumps(dict(client='unicode-fixture', action=action, **values)).encode(),
                        {'Content-Type': 'application/json', 'X-Wes-Session': generation})
                    with urllib.request.urlopen(request, timeout=10) as response: return json.load(response)

                identity = call('start')['id']
                cursor = 0

                def drain(marker=None):
                    nonlocal cursor
                    data = b''
                    deadline = time.monotonic() + 5
                    while time.monotonic() < deadline:
                        frame = call('poll', id=identity, cursor=cursor, wait_ms=150)
                        assert frame['start'] == cursor and not frame['problem'], frame
                        chunk = base64.b64decode(frame['data'])
                        assert frame['next'] == cursor + len(chunk)
                        cursor = frame['next']
                        data += chunk
                        if marker is None:
                            if data and not chunk: return data
                        elif re.search(rb'(?:^|\r?\n)' + re.escape(marker) + rb'\r?\n', data):
                            return data
                    raise AssertionError('terminal output timed out: ' + repr(data))

                startup = drain()
                assert b'\x1b[7m%' not in startup, repr(startup)
                # A normal shell prompt may contain %. Only the inverse partial-line marker is absent.
                call('resize', id=identity, cols=47, rows=18)
                call('write', id=identity, text="PS1='WES_PROMPT> '" + FINISH)
                drain(COMPLETED)
                # Leave a gap longer than a poll between the input echo and output.
                # A quiet poll is not evidence that the command has completed.
                call('write', id=identity, text="sleep 0.25; printf 'RESULT:%s:END\\n' '")
                for character in TEXT + 'X':
                    call('write', id=identity, text=character)
                # Erase ASCII X and then the complete UTF-8 ü; restore ü, finish and run.
                call('write', id=identity, text="\x7f\x7fü'" + FINISH)
                typed = drain(COMPLETED)
                decoded = typed.decode('utf-8', errors='strict')
                assert ('RESULT:' + TEXT + ':END') in decoded, repr(decoded)
                assert '<009f>' not in decoded and '<009e>' not in decoded, repr(decoded)
                assert b'\x1b[7m%' not in typed
                call('write', id=identity, text="printf 'PERCENT:%s:END\\n' '100%'" + FINISH)
                percent = drain(COMPLETED)
                assert b'PERCENT:100%:END' in percent, repr(percent)
                call('write', id=identity, text="printf 'NUMERIC:%s:END\\n' \"${LC_NUMERIC:-$LANG}\"" + FINISH)
                categories = drain(COMPLETED)
                if locale.get('LC_ALL') == 'C' or locale.get('LC_NUMERIC') == 'C':
                    assert b'NUMERIC:C:END' in categories, repr(categories)
                call('close', id=identity)
                print('PASS locale=' + repr(locale) + ': Turkish input/backspace, valid UTF-8, normal percent output, no inverse startup marker')
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
    parser.add_argument('--binary', type=Path, default=Path(__file__).resolve().parents[2] / 'target/debug/wes')
    args = parser.parse_args()
    for locale in ({}, {'LANG': 'C'}, {'LC_ALL': 'C', 'LC_CTYPE': 'en_US.UTF-8'},
                   {'LC_CTYPE': 'invalid.UTF-8', 'LC_NUMERIC': 'C'},
                   {'LANG': 'invalid.UTF-8'}, {'LC_ALL': 'invalid.UTF-8'}):
        check(args.binary.resolve(), locale)


if __name__ == '__main__': main()
