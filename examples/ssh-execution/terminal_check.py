"""Application + generic terminal manager + target admission acceptance; synthetic fixtures only."""
import base64
import json
from pathlib import Path
import selectors
import signal
import subprocess
import time
import urllib.error
import urllib.request
import uuid


def check_terminal(binary, root, recipe, real):
    recipe_file = root / 'terminal.yaml'
    recipe_file.write_text(json.dumps(recipe))
    home = root / 'terminal-home'
    # Persist the actual standalone-target recipe, then verify restored authority is held.
    prepare = subprocess.run([str(binary), '--home', str(home), '--env-file', str(recipe_file), '--command', ':calc { return 0; }'], capture_output=True, text=True, timeout=30)
    assert prepare.returncode == 0, prepare.stderr
    with (root / 'terminal-server.log').open('w+') as log:
        server = subprocess.Popen([str(binary), '--home', str(home), '--serve', '0'], cwd=root, stdout=subprocess.PIPE, stderr=log, text=True)
        events = None
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(server.stdout, selectors.EVENT_READ)
                assert selector.select(20), 'server startup timeout'
            url = server.stdout.readline().strip().removeprefix('Listening at ')
            events = urllib.request.urlopen(url + '/events', timeout=15)
            def event_until(predicate):
                while True:
                    line = events.readline().decode()
                    assert line, 'event stream ended'
                    if line.startswith('data:'):
                        event = json.loads(line[5:])
                        if predicate(event):
                            return event
            generation = event_until(lambda event: event['event'] == 'session')['generation']
            def post(path, body):
                request = urllib.request.Request(url + path, json.dumps(body).encode(), {'Content-Type':'application/json','X-Wes-Session':generation})
                with urllib.request.urlopen(request, timeout=20) as response:
                    return json.load(response)
            def source(text):
                cell = str(uuid.uuid4())
                post('/submit', {'request':'submit','cell':cell,'client':'terminal-qa','text':text})
                report = event_until(lambda event: event['event'] == 'reported' and event.get('cell') == cell)
                assert not any(d.get('severity') == 'error' for d in report.get('diagnostics', [])), report
            def terminal(action, **values):
                return post('/terminals', {'action':action,'client':'terminal-qa',**values})
            def refused(target):
                try:
                    terminal('start', target=target)
                except urllib.error.HTTPError as error:
                    assert error.code == 400
                    return error.read().decode()
                raise AssertionError('target should have been refused')
            source(':env use "ssh_demo"')
            choice = terminal('targets')['targets'][0]
            target = {key:choice[key] for key in ('environment','revision','target')}
            assert not choice['available'] and 'disabled' in refused(target)
            source(':env enable "ssh_demo"')
            assert terminal('targets')['targets'][0]['available']
            assert terminal('resolve')['target'] == target
            assert terminal('resolve', environment='ssh_demo')['target'] == target
            assert terminal('resolve', target='remote')['target'] == target
            stale_revision = 'sha256:' + '0'*64
            review = terminal('start', target={**target, 'revision':stale_revision})
            # A changed target requires review, not an admitted terminal. The
            # returned current revision can only be used by a separate start.
            assert set(review) == {'review'}, review
            assert review['review']['previousRevision'] == stale_revision, review
            assert review['review']['target'] == target, review
            assert review['review']['previousAvailable'] is False, review
            if not real:
                assert not (root / 'argv').exists(), 'review launched the client'
            result = terminal('start', target=target)
            assert not result['workspace_tools'] and result['cwd'] is None
            identity, cursor = result['id'], 0
            def until(marker=None, closed=False):
                nonlocal cursor
                output = ''
                deadline = time.monotonic() + 12
                while time.monotonic() < deadline:
                    frame = terminal('poll', id=identity, cursor=cursor, wait_ms=100)
                    cursor = frame['next']
                    output += base64.b64decode(frame['data']).decode('utf-8','replace')
                    if (closed and frame['closed']) or (marker and marker in output):
                        return output, frame
                raise AssertionError(('terminal output timeout',marker,output))
            # Commands travel as terminal bytes. Neither startup nor output carries local tools.
            terminal('write', id=identity, text="printf '\\nPROOF:%s:%s:%s\\n' \"${WES_BRIDGE_TOKEN-unset}\" \"${WES_BRIDGE_URL-unset}\" \"$WES_EXAMPLE\"\r")
            until("\r\nPROOF:unset:unset:literal '$HOME; value\r\n")
            if real:
                terminal('resize', id=identity, cols=91, rows=27)
                terminal('write', id=identity, text='stty size\r')
                until('\r\n27 91\r\n')
            terminal('write', id=identity, text="printf '\\nPATH:%s\\n' \"$PWD\"\r")
            output, _ = until('\r\nPATH:')
            assert str(root) in output or str(root.resolve()) in output, output
            # Selecting another environment never moves an already admitted session.
            source(':env use "default"')
            assert terminal('resolve', environment='ssh_demo')['target'] == target
            assert terminal('resolve')['target']['environment'] == 'default'
            terminal('write', id=identity, text="printf '\\nFROZEN:%s\\n' \"$WES_EXAMPLE\"\r")
            until("\r\nFROZEN:literal '$HOME; value\r\n")
            # A normal shell exit is observed, with no uncertainty notice.
            terminal('write', id=identity, text='exit 7\r')
            _, frame = until(closed=True)
            assert frame['exit'] == 7 and not frame['problem'], frame
            terminal('close', id=identity)
            result = terminal('start', target=target)
            identity, cursor = result['id'], 0
            terminal('write', id=identity, text="printf '\\nREADY-TO-STOP\\n'\r")
            until('\r\nREADY-TO-STOP\r\n')
            source(':env disable "ssh_demo"')
            _, frame = until(closed=True)
            assert 'ENV036' in frame['problem'], frame
            assert 'disabled' in refused(target)
            terminal('close', id=identity)
            if real:
                source(':env enable "ssh_demo"')
                Path(recipe['targets']['remote']['known_hosts']).write_text('')
                result = terminal('start', target=target)
                identity, cursor = result['id'], 0
                _, frame = until(closed=True)
                assert frame['exit'] == 255 and 'ENV036' in frame['problem'], frame
                terminal('close', id=identity)
        finally:
            if events:
                events.close()
            server.send_signal(signal.SIGINT)
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill(); server.wait(timeout=5)
    print('PASS target terminal: standalone recipe, held restart, capability discovery, stale review without launch, disabled admission, input, cwd/env, frozen destination, exit and joined disable' + (', actual OpenSSH PTY resize and host-key refusal' if real else ', synthetic client'))
