#!/usr/bin/env python3
"""Check actual example files via normal submit, then restore without source archives."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import urllib.request
import uuid

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
import lifecycle
from fixture_environment import isolated
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, required=True)
binary = parser.parse_args().binary.resolve()
types = (HERE / 'types.yaml').read_text()
environments = (HERE / 'environments.yaml').read_text()
with tempfile.TemporaryDirectory(prefix='wes-document-input-') as directory:
    root = Path(directory).resolve()
    user = root / 'user'; user.mkdir()
    home = root / 'home'
    env = isolated(user)
    with lifecycle.Owned([binary, '--home', home, '--serve', '0'], cwd=root, env=env,
                         log=root / 'server.log', patience=30) as app:
        events = None
        try:
            line = app.line(45)
            assert line.startswith('Listening at http://'), line
            url = line.removeprefix('Listening at ')
            events = urllib.request.urlopen(url + '/events', timeout=20)
            def until(predicate):
                while True:
                    line = events.readline().decode()
                    assert line, 'event stream ended'
                    if line.startswith('data:'):
                        event = json.loads(line[5:])
                        if predicate(event): return event
            generation = until(lambda e: e['event'] == 'session')['generation']
            def submit(text, document=None):
                cell = str(uuid.uuid4())
                data = {'request': 'submit', 'client': 'example', 'cell': cell, 'text': text}
                if document is not None: data['document'] = {'source': document}
                request = urllib.request.Request(url + '/submit', json.dumps(data).encode(),
                    {'Content-Type': 'application/json', 'X-Wes-Session': generation})
                with urllib.request.urlopen(request, timeout=20) as reply: assert reply.status == 202
                planned = until(lambda e: e['event'] == 'planned' and e['cell'] == cell)
                assert not planned.get('failure'), planned
                assert not any(d['severity'] == 'error' for d in planned['diagnostics']), planned
                if document is not None: assert planned['document']['source'] == document
                return planned
            submit(':package load source:"" origin:"editor:example-types"', types)
            plan = submit(':env plan source:"" origin:"editor:example-environments" > reviewed', environments)
            assert not plan['nodes']
            assert any(d['code'] == 'ENV000' for d in plan['diagnostics'])
            submit(':env apply $reviewed')
        finally:
            if events: events.close()
            code = app.stop()
            assert code == 0 and not app.forced, app.log()
    shutil.rmtree(home / 'imports')
    def command(text, selected_home=home):
        reply = subprocess.run([str(binary), '--home', str(selected_home), '--command', text],
            cwd=root, env=env, capture_output=True, text=True, timeout=30)
        assert reply.returncode == 0, reply.stdout + reply.stderr
        return reply.stdout + reply.stderr
    assert 'EditorGreeting' in command(':list types')
    assert 'editor_demo' in command(':inspect env:editor_demo')
    assert not (home / 'imports').exists(), 'restore performed a new capture'
    # Public CLI text literals use command escapes, never literal statement newlines.
    def quote(text):
        return '"' + text.replace('\\', '\\\\').replace('"', '\\"').replace('\n', '\\n').replace('\r', '\\r').replace('\t', '\\t') + '"'
    command(':package load source:' + quote(types), root / 'cli-home')
    assert 'EditorGreeting' in command(':list types', root / 'cli-home')
print('PASS: actual multilingual YAML text, explicit plan/apply, archived identity and offline replay')
