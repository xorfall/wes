#!/usr/bin/env python3
"""Actual OpenAPI extraction, final-target evidence and Rust HTTP contracts."""
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
from threading import Thread
from server import make_server

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def at(document, pointer):
    value = document
    for segment in pointer[2:].split('/'):
        key = segment.replace('~1', '/').replace('~0', '~')
        value = value[int(key)] if isinstance(value, list) else value[key]
    return value


with tempfile.TemporaryDirectory(prefix='wes-schema-provenance-') as directory:
    root = Path(directory)
    generated = subprocess.run([
        str(ROOT/'tools/describe/wes-extract'), '-provider', 'example_api',
        '-from', str(HERE/'openapi.json'),
        '-out', str(root/'example.json')], capture_output=True, text=True, timeout=30)
    assert generated.returncode == 0, generated.stderr
    document = json.loads((root/'example.json').read_text())
    entries = document['source']['provenance']['entries']
    assert document['source']['sha256'] == hashlib.sha256((HERE/'openapi.json').read_bytes()).hexdigest()
    for entry in entries:
        at(document, entry['target'])  # Every final pointer must resolve in the actual output.
    assert any(e['target'] == '#/operations/0/authOptions' and e['basis'] == 'documented' for e in entries)
    assert any(e['target'].endswith('/fields/id/type') and e['basis'] == 'documented' and e['pointer'] for e in entries)
    assert any(e['target'].endswith('/fields/id/optional') and e['basis'] == 'inferred' for e in entries)
    assert document['operations'][0]['auth'] == []
    assert document['operations'][0]['authOptions'] == [{'schemes': [], 'auth': []}]
    server = make_server(0)
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        for scenario, status, state in [('ok', 200, 'validated'), ('denied', 401, 'undocumented'), ('wrong-status', 201, 'undocumented'), ('wrong-shape', 200, 'mismatch')]:
            recipe = (HERE/'environments.yaml').read_text().replace(
                'http://127.0.0.1:8783/ok', f'http://127.0.0.1:{server.server_port}/{scenario}')
            (root/'environments.yaml').write_text(recipe)
            result = subprocess.run([str(ROOT/'target/debug/wes'), '--home', str(root/scenario),
                '--env-file', str(root/'environments.yaml'), '--env', 'demo', '--file', str(HERE/'call.wes')],
                cwd=root, capture_output=True, text=True, timeout=30)
            assert result.returncode == 0, result.stdout + result.stderr
            reports = [json.loads(line.partition(': ')[2]) for line in result.stdout.splitlines()
                       if line.startswith('id') and ': ' in line]
            response = reports[-1]
            assert response['status'] == status, response
            assert response['validation']['state'] == state, response
            if scenario == 'ok':
                assert response['body'] == [{'id': 1}], response
            elif scenario == 'denied':
                assert response['body']['error'] == 'Synthetic authorization failure', response
            elif scenario == 'wrong-shape':
                assert response['validation']['issues'], response
                assert response['body'] == [{'id': 'wrong type'}], response
        assert [p for p, _ in server.requests] == ['/ok/items', '/denied/items', '/wrong-status/items', '/wrong-shape/items']
        assert all(auth is None for _, auth in server.requests), 'Public operation attached credentials'
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
print('PASS public auth, field provenance, optional schema fields, real 401 and strict response contracts')
