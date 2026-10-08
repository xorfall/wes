"""Small example client for the existing loopback API; no browser or user workspace access."""
from collections import deque
import json
from pathlib import Path
import sys
import threading
import time
import urllib.error
import urllib.request
import uuid

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE.parent))
import lifecycle


def copy_project(root, port):
    for path in HERE.iterdir():
        if path.suffix in ('.yaml', '.json', '.wes'):
            (root / path.name).write_text(path.read_text().replace(':8771', f':{port}'))


class Client:
    def __init__(self, binary, root, site=False, environment="monitor-demo"):
        self.environment = environment
        self.changed = threading.Condition()
        self.events = deque(maxlen=2000)
        self.names, self.ready = {}, {}
        self.generation = None
        self.context = None
        self.problem = None
        self.closed = False
        self.serial = 0
        self.root = root
        args = [str(binary), '--home', str(root / 'home'), '--serve', '0', '--no-auto-keep']
        if site:
            args += ['--site', str(ROOT / 'gui/dist')]
        self.owned = lifecycle.Owned(args, cwd=root, log=root / 'engine.stderr')
        self.process = self.owned.process
        try:
            line = self.owned.line(30)
            if not line.startswith('Listening at http://'):
                raise RuntimeError(line + self.owned.log())
            self.url = line.removeprefix('Listening at ')
            self.stream = urllib.request.urlopen(self.url + '/events', timeout=30)
            self.reader = threading.Thread(target=self._read, daemon=True)
            self.reader.start()
            self.wait(lambda: self.generation)
        except BaseException:
            self.close()
            raise

    def _read(self):
        try:
            for raw in self.stream:
                if not raw.startswith(b'data:'):
                    continue
                event = json.loads(raw[5:])
                with self.changed:
                    self.serial += 1
                    event['_serial'] = self.serial
                    self.events.append(event)
                    kind = event['event']
                    if kind == 'session':
                        self.generation = event['generation']
                        self.names.clear()
                        self.ready.clear()
                    elif kind == 'environments':
                        self.context = ({'selected': self.environment, 'revisions': event['revisions']}
                                        if self.environment in event['revisions'] else None)
                    elif kind == 'created' and event.get('name'):
                        self.names[event['name']] = event['node']
                    elif kind in ('ready', 'evidence'):
                        self.ready[event['node']] = event
                    elif kind == 'reported' and any(d.get('severity') == 'error' for d in event.get('diagnostics', [])):
                        self.problem = str(event)
                    elif kind == 'failed':
                        self.problem = str(event)
                    self.changed.notify_all()
        except Exception as error:
            with self.changed:
                if not self.closed:
                    self.problem = repr(error)
                self.changed.notify_all()

    def wait(self, predicate, timeout=20):
        deadline = time.monotonic() + timeout
        with self.changed:
            while True:
                if self.problem:
                    raise AssertionError(self.problem)
                value = predicate()
                if value:
                    return value
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise AssertionError(f'Timed out; recent events: {list(self.events)[-12:]}')
                self.changed.wait(min(remaining, .1))

    def submit(self, text, switch=False):
        cell = str(uuid.uuid4())
        before = self.generation
        self.post({
            'request': 'submit', 'cell': cell, 'text': text,
            'client': 'live-monitor-example', 'environments': self.context,
        })
        result = self.wait(lambda: next((e for e in self.events if e['event'] == 'planned' and e['cell'] == cell), None)
                           or ({'generation': self.generation} if switch and self.generation != before else None))
        assert not result.get('failure'), result
        return result

    def post(self, payload):
        request = urllib.request.Request(self.url + '/submit', json.dumps(payload).encode(),
                                         {'Content-Type': 'application/json', 'X-Wes-Session': self.generation})
        with urllib.request.urlopen(request, timeout=20) as response:
            response.read()

    def keep(self, name):
        frame = self.wait(lambda: self.ready.get(self.names.get(name)))
        self.post({'request': 'keep', 'handle': frame['handle']})
        self.wait(lambda: self.ready.get(frame['node'], {}).get('kept'))

    def file(self, name):
        return self.submit((self.root / name).read_text())

    def value(self, name, predicate=lambda _: True, after=0, timeout=20):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            def fresh():
                event = self.ready.get(self.names.get(name))
                return event if event and event['_serial'] > after else None
            event = self.wait(fresh, timeout=max(.1, deadline-time.monotonic()))
            try:
                with urllib.request.urlopen(self.url + '/values/' + event['handle'], timeout=10) as response:
                    value = json.load(response)
                if predicate(value['data']):
                    return value
                after = event['_serial']
                continue
            except urllib.error.HTTPError as error:
                if error.code not in (404, 409, 410, 429, 503):
                    raise
            with self.changed:
                self.changed.wait(.03)
        raise AssertionError(f'Timed out reading {name}; last frame: {event}')

    def prepare(self):
        # Server-mode environments belong to clients, not batch startup flags.
        self.submit(':env plan file:environments.yaml > monitor_plan')
        self.submit(':env apply $monitor_plan')
        self.wait(lambda: self.context)
        self.submit(':env use "monitor-demo"')
        self.submit(':package load path:types.yaml')
        self.submit(':workspace policy mode:reactive')
        self.file('monitor.wes')
        self.file('calc-table.wes')
        self.value('health_display')

    def close(self):
        self.closed = True
        try:
            self.owned.finish()
        finally:
            if hasattr(self, 'stream'):
                self.stream.close()
                if self.reader.ident is not None:
                    self.reader.join(timeout=5)
                if self.reader.is_alive():
                    raise RuntimeError('monitor event reader did not stop')
