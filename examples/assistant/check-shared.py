#!/usr/bin/env python3
"""Two real PTY/MCP participants share user work without a model or live user data."""
import sys
import argparse, base64, json, os, selectors, shlex, subprocess, tempfile, time, urllib.request

# Publish synchronization evidence only after the complete JSON is visible.
def publish(path, text):
    staged = path.with_suffix('.pending')
    staged.write_text(text)
    staged.replace(path)

from pathlib import Path
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
from fixture_environment import isolated
ROOT = HERE.parents[1]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/wes')
    binary = parser.parse_args().binary.resolve()
    with tempfile.TemporaryDirectory(prefix='wes-shared-agent-') as directory:
        root = Path(directory)
        env = {k:v for k,v in os.environ.items() if not any(x in k for x in ['API_KEY', 'TOKEN', 'SECRET', 'PASSWORD'])}
        env = isolated(root, env)
        with (root / 'server.log').open('w+') as errors:
            app = subprocess.Popen([str(binary), '--home', str(root / 'home'), '--serve', '0', '--no-auto-keep'], cwd=root, env=env, stdout=subprocess.PIPE, stderr=errors, text=True)
            events = None
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(app.stdout, selectors.EVENT_READ)
                    assert selector.select(30), 'server startup timeout'
                url = app.stdout.readline().strip().removeprefix('Listening at ')
                events = urllib.request.urlopen(url + '/events', timeout=15)
                generation = None
                while not generation:
                    line = events.readline().decode()
                    if line.startswith('data:'):
                        event = json.loads(line[5:])
                        if event['event'] == 'session': generation = event['generation']
                def post(path, body, workspace=None, scoped_generation=None):
                    headers = {'Content-Type': 'application/json', 'X-Wes-Session': scoped_generation or generation}
                    if workspace: headers['X-Wes-Workspace'] = workspace
                    request = urllib.request.Request(url + path, json.dumps(body).encode(), headers)
                    with urllib.request.urlopen(request, timeout=15) as response:
                        text = response.read(); return json.loads(text) if text else None
                post('/submit', {'request': 'submit', 'client': 'synthetic-user', 'cell': 'human-seed', 'text': ':calc { return {seed: 41}; } > user_seed'})
                while True:
                    line = events.readline().decode()
                    if line.startswith('data:') and json.loads(line[5:])['event'] == 'ready': break
                def terminal(action, **kwargs): return post('/terminals', {'client': 'synthetic-user', 'action': action, **kwargs}, 'default')
                identities = {role: terminal('start')['id'] for role in ['a', 'b']}
                cursor = dict.fromkeys(identities, 0); output = dict.fromkeys(identities, '')
                for role, identity in identities.items():
                    command = 'python3 ' + shlex.quote(str(HERE / 'shared_agent.py')) + ' ' + role + ' ' + shlex.quote(str(root)) + '\r'
                    terminal('write', id=identity, text=command)
                closed = False; reloaded = False; tab_requests = []
                end = time.monotonic() + 60
                while time.monotonic() < end:
                    for role, identity in identities.items():
                        if role == 'a' and closed: continue
                        frame = terminal('poll', id=identity, cursor=cursor[role], wait_ms=20)
                        cursor[role] = frame['next']; output[role] += base64.b64decode(frame['data']).decode('utf-8', 'replace')
                        if frame.get('ui'):
                            request = frame['ui']; operation = request['operation']
                            assert operation['kind'] in ['layout_read', 'tab_open'], request
                            if operation['kind'] == 'tab_open':
                                args = {key: value for key, value in operation.items() if key != 'kind'}
                                assert args == {'workspace': 'shared-analysis', 'pane': 'p1', 'activate': False}
                                tab_requests.append(args)
                            terminal('uireply', id=identity, request=request['id'], result={'ok': True, 'layout': {'panes': [{'id': 'p1'}]}})
                    if not closed and 'SHARED_A_READY\r\n' in output['a']:
                        terminal('close', id=identities['a']); closed = True
                        publish(root / 'a-closed.json', '{}')
                    if not reloaded and (root / 'b-ready-reload.json').exists():
                        joined = json.loads((root / 'a-ready.json').read_text())
                        # Reload is an explicit primary-workspace control. Selecting the target
                        # keeps origin-bound terminals alive, then permits its own reload.
                        post('/submit', {'request': 'submit', 'client': 'synthetic-user', 'cell': 'select-shared', 'text': ':workspace load "shared-analysis"'})
                        while True:
                            line = events.readline().decode()
                            if line.startswith('data:'):
                                event = json.loads(line[5:])
                                if event['event'] == 'session' and event['generation'] == joined['generation']: break
                                if event['event'] == 'planned' and event.get('cell') == 'select-shared':
                                    assert not any(d.get('severity') == 'error' for d in event.get('diagnostics', [])), event
                        post('/submit', {'request': 'submit', 'client': 'synthetic-user', 'cell': 'reload-shared', 'text': ':workspace load "shared-analysis"'}, 'shared-analysis', joined['generation'])
                        # Wait on the generation through the existing explicit open route.
                        for _ in range(200):
                            opened = post('/workspaces', {'name': 'shared-analysis', 'create': False})
                            if opened['generation'] != joined['generation']: break
                            time.sleep(.01)
                        assert opened['generation'] != joined['generation'], opened
                        publish(root / 'reloaded.json', '{}'); reloaded = True
                    if 'SHARED_B_OK\r\n' in output['b']: break
                    if any('Traceback (most recent call last)' in text for text in output.values()): raise AssertionError(output)
                assert closed and reloaded and 'SHARED_B_OK\r\n' in output['b'], output
                assert len(tab_requests) == 1
                terminal('close', id=identities['b'])
                print('PASS: shared user cell, two independent MCP owners, explicit join/context, target isolation, pane request, departure and reload revocation')
            finally:
                if events: events.close()
                app.terminate()
                try: app.wait(timeout=15)
                except subprocess.TimeoutExpired: app.kill(); app.wait()
                if app.returncode != 0 and app.returncode != -15:
                    errors.seek(0); raise AssertionError(errors.read())

if __name__ == '__main__': main()
