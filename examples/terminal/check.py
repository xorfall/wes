#!/usr/bin/env python3
"""Actual server + PTY + child/child CLI test, entirely isolated from user workspaces."""
import argparse
import base64
import json
import os
import re
from pathlib import Path
import selectors
import signal
import subprocess
import tempfile
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


class Api(BaseHTTPRequestHandler):
    def do_GET(self):
        self.server.calls.append(self.path)
        if self.server.delay:
            self.server.entered.set()
            assert self.server.release.wait(10)
        body = {'temperature': self.server.temperature, 'humidity': 45, 'calibration': 0.375}
        if self.path == '/status':
            body = {'station': 'sensor-room', 'readings': body}
        data = json.dumps(body).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *_):
        pass


def api(temperature):
    server = ThreadingHTTPServer(('127.0.0.1', 0), Api)
    server.calls, server.temperature, server.delay = [], temperature, False
    server.entered, server.release = threading.Event(), threading.Event()
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server, thread


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/wes')
    binary = parser.parse_args().binary.resolve()
    servers = [api(21.25), api(24.75)]
    process = None
    try:
        with tempfile.TemporaryDirectory(prefix='wes-terminal-qa-') as name:
            temporary = Path(name)
            (temporary / 'lab-sensors.json').write_bytes((HERE / 'lab-sensors.json').read_bytes())
            recipe = (HERE / 'environments.yaml').read_text()
            for old, (server, _) in zip([8765, 8766], servers):
                recipe = recipe.replace(f':{old}', f':{server.server_port}')
            (temporary / 'environments.yaml').write_text(recipe)
            errors = open(temporary / 'engine.stderr', 'w+')
            env = dict(os.environ, WES_FIXTURE_SECRET='synthetic-must-not-inherit')
            process = subprocess.Popen([str(binary), '--home', str(temporary / 'home'), '--serve', '0', '--no-auto-keep'], cwd=temporary, env=env, stdout=subprocess.PIPE, stderr=errors, text=True)
            selector = selectors.DefaultSelector()
            selector.register(process.stdout, selectors.EVENT_READ)
            assert selector.select(20), 'server did not announce startup'
            line = process.stdout.readline().strip()
            if not line.startswith('Listening at http://'):
                errors.flush(); errors.seek(0)
                raise AssertionError(line + errors.read())
            url = line.removeprefix('Listening at ')
            events = urllib.request.urlopen(url + '/events', timeout=10)
            generation = None
            while not generation:
                line = events.readline().decode()
                if line.startswith('data:'):
                    event = json.loads(line[5:])
                    if event['event'] == 'session':
                        generation = event['generation']
            client, context = 'synthetic-agent-ui', None

            def post(path, data, gen=None):
                request = urllib.request.Request(url + path, json.dumps(data).encode(), {'Content-Type': 'application/json', 'X-Wes-Session': gen or generation})
                with urllib.request.urlopen(request, timeout=25) as response:
                    return json.load(response)

            def source(text, console=False):
                import uuid
                return post('/submit', {'request': 'submit', 'cell': str(uuid.uuid4()), 'text': text, 'client': client, 'environments': context, 'console': console})

            def event_until(predicate):
                while True:
                    line = events.readline().decode()
                    assert line, 'event stream ended'
                    if line.startswith('data:'):
                        event = json.loads(line[5:])
                        if event['event'] == 'reported' and any(d.get('severity') == 'error' for d in event.get('diagnostics', [])):
                            raise AssertionError(event)
                        if predicate(event):
                            return event

            def ready_named(name):
                created = event_until(lambda e: e['event'] == 'created' and e.get('name') == name)
                return event_until(lambda e: e['event'] == 'ready' and e['node'] == created['node'])

            source(':env plan file:"environments.yaml" > initial')
            source(':env apply $initial')
            environment = event_until(lambda e: e['event'] == 'environments' and 'demo' in e['revisions'])
            context = {'selected': 'demo', 'revisions': environment['revisions']}
            source(':env use "demo"')
            source((HERE / 'prepare.wes').read_text())
            ready_named('readings')

            def terminal(action, **kwargs):
                return post('/terminals', {'client': client, 'action': action, **kwargs})

            # Restore a terminal directory without restoring or replaying a previous command.
            restored_dir = temporary / 'saved directory'
            restored_dir.mkdir()
            restored = terminal('start', cwd=str(restored_dir.resolve()))
            assert restored['cwd'] == str(restored_dir.resolve()), restored
            terminal('write', id=restored['id'], text="pwd > restored-cwd.txt\r")
            # Shell redirection creates the file before pwd writes it. Wait for the complete
            # line, not just file existence, so the check cannot read an empty in-flight file.
            until = time.monotonic() + 10
            restored_output = ''
            while time.monotonic() < until:
                try:
                    restored_output = (restored_dir / 'restored-cwd.txt').read_text()
                except FileNotFoundError:
                    pass
                if restored_output.endswith('\n'):
                    break
                time.sleep(.03)
            assert restored_output.strip() == str(restored_dir.resolve()), repr(restored_output)
            terminal('close', id=restored['id'])
            for invalid in ['relative-directory', str(temporary / 'missing-directory')]:
                try:
                    terminal('start', cwd=invalid)
                    raise AssertionError('invalid saved terminal directory was accepted')
                except urllib.error.HTTPError as e:
                    assert e.code == 400
            print('PASS pane directories: fresh shell uses saved cwd; invalid cwd is refused')

            identity = terminal('start')['id']
            try:
                post('/terminals', {'client': 'wrong-client', 'action': 'poll', 'id': identity, 'cursor': 0})
                raise AssertionError('another client could read the terminal')
            except urllib.error.HTTPError as e:
                assert e.code == 410
                unavailable_body = e.read()
            cursor, output = 0, ''

            def write(text):
                terminal('write', id=identity, text=text)

            def wait_for(expected):
                nonlocal cursor, output
                until = time.monotonic() + 20
                while time.monotonic() < until:
                    frame = terminal('poll', id=identity, cursor=cursor)
                    cursor = frame['next']
                    output += base64.b64decode(frame['data']).decode('utf-8', 'replace')
                    if re.search(re.escape(expected) + r'\r*\n', output):
                        return
                    assert not frame['closed'], output + str(frame)
                    time.sleep(.03)
                raise AssertionError(f'missing {expected!r}: {output[-8000:]}')

            write('stty -echo\r')
            time.sleep(.1)
            write(f'python3 "{HERE / "agent.py"}"\r')
            wait_for('SYNTHETIC_AGENT_OK')
            assert servers[0][0].calls == ['/readings', '/status', '/readings'], servers[0][0].calls
            print('PASS T01–T04: real PTY -> synthetic agent -> child shell -> help, call, typed/current value; no re-run')
            def pane_request(command, expected, error=None):
                nonlocal cursor, output
                write(command + "; printf 'PANE_EXIT=%s\\n' $?\r")
                until = time.monotonic() + 10
                while time.monotonic() < until:
                    frame = terminal('poll', id=identity, cursor=cursor)
                    cursor = frame['next']
                    output += base64.b64decode(frame['data']).decode('utf-8', 'replace')
                    if frame.get('command'):
                        request = frame['command']
                        break
                    time.sleep(.02)
                else:
                    raise AssertionError('No pane request: ' + output)
                assert request['text'] == expected, request
                other = terminal('start')['id']
                assert not terminal('commandclaim', id=other, request=request['id'])['claimed']
                terminal('close', id=other)
                try:
                    post('/terminals', {'client': 'wrong-client', 'action': 'commandclaim', 'id': identity, 'request': request['id']})
                    raise AssertionError('another client claimed a pane command')
                except urllib.error.HTTPError as e:
                    assert e.code == 410
                    assert e.read() == unavailable_body
                assert terminal('commandclaim', id=identity, request=request['id'])['claimed']
                assert not terminal('commandclaim', id=identity, request=request['id'])['claimed']
                assert terminal('poll', id=identity, cursor=cursor).get('command') is None
                terminal('commandreply', id=identity, request=request['id'], error=error)
                wait_for('PANE_EXIT=' + ('2' if error else '0'))
                output = ''

            pane_request('wesx --cmd "/rsplit xterm"', '/rsplit xterm')
            pane_request('wesx --cmd "/split"', '/split', 'Four panes are already open.')
            pane_request((HERE/'tab.sh').read_text().strip(), '/terminal-tab')
            pane_request('wesx --cmd "/terminal-tab"', '/terminal-tab')
            pane_request('wesx --cmd "/terminal-tab"', '/terminal-tab', 'Four terminals are already open.')
            pane_request('wesx exit', '/close')  # The synthetic UI acknowledges; React tests verify closure.
            write("wesx --cmd 'engine source'; printf 'INVALID_CMD=%s\\n' $?\r")
            wait_for('INVALID_CMD=2')
            write("wesx --help; printf 'HELP_DONE\\n'\r")
            wait_for('HELP_DONE')
            assert 'wesx provider' in output and 'wesx exit' in output and 'wesx tab' in output
            print('PASS wesx: provider/value compatibility, pane routing, single-use claims, ownership, errors and exit request')
            terminal('resize', id=identity, cols=111, rows=37)
            write("stty size; printf 'RESIZE_DONE\\n'\r")
            wait_for('RESIZE_DONE')
            assert '37 111' in output, output
            write("read reply; printf 'INPUT=%s\\n' \"$reply\"\r")
            write('hello\r')
            wait_for('INPUT=hello')
            write("sleep 20\r")
            time.sleep(.1)
            write('\x03')
            write("printf 'INTERRUPT_DONE\\n'\r")
            wait_for('INTERRUPT_DONE')
            print('PASS T09: TTY inheritance, input, resize, interrupt and resumed shell prompt')
            # A pending call captures demo; changing the UI context cannot retarget it.
            server = servers[0][0]
            server.delay = True
            write("lab-sensors readings; printf 'PINNED_DONE\\n'\r")
            assert server.entered.wait(10), output
            context = {'selected': 'alternate', 'revisions': environment['revisions']}
            source(':env use "alternate"')
            server.delay = False
            server.release.set()
            wait_for('PINNED_DONE')
            before = len(servers[1][0].calls)
            write("lab-sensors readings; printf 'CONTEXT_DONE\\n'\r")
            wait_for('CONTEXT_DONE')
            assert len(servers[1][0].calls) == before + 1, output
            assert '24.75' in output
            print('PASS T05: live selected environment and pinned in-flight call')
            addition = '    imports:\n      new-data:\n        source: {kind: spec, file: lab-sensors.json}\n        bind: {target: local, endpoint: http://127.0.0.1:%s}\n' % server.server_port
            (temporary / 'updated.yaml').write_text(recipe.replace('    imports:\n', addition, 1))
            source(':env plan file:"updated.yaml" > updated')
            source(':env apply $updated')
            environment = event_until(lambda e: e['event'] == 'environments' and e['revisions']['demo'] != environment['revisions']['demo'])
            context = {'selected': 'demo', 'revisions': environment['revisions']}
            source(':env use "demo"')
            write("while ! command -v new-data >/dev/null; do sleep .1; done; new-data readings; printf 'NEW_IMPORT_DONE\\n'\r")
            wait_for('NEW_IMPORT_DONE')
            assert server.calls[-1] == '/readings'
            print('PASS T05/T06: live new provider executable and native command collision fallback')
            context = {'selected': 'private', 'revisions': environment['revisions']}
            source(':env use "private"')
            source('lab-sensors readings > hidden')
            ready_named('hidden')
            write("wesx value get hidden; printf 'PRIVATE_EXIT=%s\\n' $?\r")
            wait_for('PRIVATE_EXIT=1')
            write("wesx value get missing; printf 'MISSING_EXIT=%s\\n' $?\r")
            wait_for('MISSING_EXIT=1')
            server.delay = True
            server.entered.clear(); server.release.clear()
            source(':refresh $readings')
            assert server.entered.wait(10)
            write("wesx value get readings; printf 'STALE_EXIT=%s\\n' $?\r")
            wait_for('STALE_EXIT=1')
            server.delay = False; server.release.set()
            print('PASS T07: private, unavailable and stale value reads fail without invoking producers')
            # Capture the synthetic delegated environment into a TEMPORARY fixture file to simulate
            # an intentionally surviving child. No real credential is read or written here.
            write("python3 -c 'import os,json; json.dump({k:v for k,v in os.environ.items() if k.startswith(\"WES_BRIDGE_\")},open(\"bridge.json\",\"w\"))'; printf 'CAPTURED\\n'\r")
            wait_for('CAPTURED')
            delegated = json.loads((temporary / 'bridge.json').read_text())
            terminal('close', id=identity)
            for unavailable_id in [identity, 'missing-terminal']:
                try:
                    terminal('poll', id=unavailable_id, cursor=0)
                    raise AssertionError('unavailable terminal remained readable')
                except urllib.error.HTTPError as e:
                    assert e.code == 410
                    assert e.read() == unavailable_body
            replacement = terminal('start')['id']
            assert replacement != identity
            terminal('close', id=replacement)
            print('PASS unavailable terminals: closed/missing/wrong-owner share 410; fresh start works')

            child = subprocess.run([str(binary), '--terminal-bridge', 'wes-value', 'get', 'readings'], env=dict(os.environ, **delegated), capture_output=True, text=True, timeout=10)
            assert child.returncode == 3 and not child.stdout, child
            print('PASS T11: closed terminal rejects surviving child authority')
            # A local synthetic bridge consumes a request, then loses its reply. The helper must
            # report uncertainty and must not replay the request automatically.
            class LostReply(BaseHTTPRequestHandler):
                def do_POST(self):
                    self.rfile.read(int(self.headers['Content-Length']))
                    self.server.effects += 1
                    self.close_connection = True
                def log_message(self, *_):
                    pass
            lost = ThreadingHTTPServer(('127.0.0.1', 0), LostReply)
            lost.effects = 0
            lost_thread = threading.Thread(target=lost.serve_forever, daemon=True)
            lost_thread.start()
            try:
                child = subprocess.run([str(binary), '--terminal-bridge', 'lab-sensors', 'status'], env=dict(os.environ, WES_BRIDGE_URL=f'http://127.0.0.1:{lost.server_port}/terminal-bridge', WES_BRIDGE_TOKEN='synthetic'), capture_output=True, text=True, timeout=10)
                assert child.returncode == 3 and not child.stdout and lost.effects == 1
            finally:
                lost.shutdown(); lost.server_close(); lost_thread.join(timeout=3)
            print('PASS T08: lost reply reports uncertainty without duplicating the consumed request')

            identity = terminal('start')['id']
            cursor, output = 0, ''
            write('stty -echo\r')
            time.sleep(.1)
            write("python3 -c 'import os,json; json.dump({k:v for k,v in os.environ.items() if k.startswith(\"WES_BRIDGE_\")},open(\"bridge2.json\",\"w\"))'; printf 'SECOND_CAPTURED\\n'\r")
            wait_for('SECOND_CAPTURED')
            delegated = json.loads((temporary / 'bridge2.json').read_text())
            original_generation = generation
            source(':workspace save "other"')
            source(':workspace load "other"')
            new_session = event_until(lambda e: e['event'] == 'session' and e['generation'] != generation)
            generation = new_session['generation']
            child = subprocess.run([str(binary), '--terminal-bridge', 'wes-provider', '--list'], env=dict(os.environ, **delegated), capture_output=True, text=True, timeout=10)
            assert child.returncode == 0 and child.stdout, 'selecting another workspace must retain the terminal owner'
            source(':workspace load "default"')
            generation = event_until(lambda e: e['event'] == 'session' and e['generation'] == original_generation)['generation']
            source(':workspace load "default"')  # Explicit replacement of the terminal's owner revokes its generation.
            generation = event_until(lambda e: e['event'] == 'session' and e['generation'] != original_generation)['generation']
            child = subprocess.run([str(binary), '--terminal-bridge', 'wes-provider', '--list'], env=dict(os.environ, **delegated), capture_output=True, text=True, timeout=10)
            assert child.returncode == 3 and not child.stdout
            print('PASS T06/T10/T11: workspace selection retains authority; owner reload revokes it; cross-client reads rejected')
            events.close()
            process.send_signal(signal.SIGINT)
            assert process.wait(timeout=15) == 0
            errors.close()
            process = None
    finally:
        if process is not None:
            process.send_signal(signal.SIGINT)
            try: process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait()
        for server, thread in servers:
            server.release.set(); server.shutdown(); server.server_close(); thread.join(timeout=3)


if __name__ == '__main__':
    main()
