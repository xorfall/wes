#!/usr/bin/env python3
"""Verify the actual OpenAPI schema, environment and scripts offline."""
import argparse
import copy
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
from threading import Thread
from server import make_server

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]


def run(command, cwd, expected=0):
    result = subprocess.run([str(s) for s in command], cwd=cwd, capture_output=True, text=True, timeout=90)
    assert result.returncode == expected, result.stdout + result.stderr
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/wes')
    args = parser.parse_args()
    binary = args.binary.resolve()
    assert binary.is_file(), 'Build wes first: cargo build -p wes --locked'
    server = make_server(0)
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='wes-readme-import-') as temporary:
            tmp = Path(temporary)
            extract = tmp / 'wes-extract'
            run(['go', 'build', '-o', extract, './cmd/extract'], ROOT / 'tools/describe')
            source = HERE / 'openapi.json'
            command = [extract, '-provider', 'catalogue', '-from', source]
            descriptor = tmp / 'catalogue.json'
            result = run(command + ['-out', descriptor], tmp)
            document = json.loads(descriptor.read_text())
            assert document['source']['sha256'] == hashlib.sha256(source.read_bytes()).hexdigest()
            assert (document['source']['discovered'], document['source']['emitted'], document['source']['skipped']) == (2, 2, 0)
            parameter = next(op for op in document['operations'] if op['route'] == '/items')['parameters'][0]
            bounds = document['types'][parameter['type']]
            assert bounds['min'] == 1 and bounds['max'] == 100 and 'enum' not in bounds
            assert not server.requests, 'extraction invoked API'
            old = descriptor.read_bytes()
            run(command + ['-out', descriptor], tmp, expected=1)
            assert descriptor.read_bytes() == old, 'reviewed output overwritten'

            # Validate stdin's complete source through the same deterministic path.
            stdin = subprocess.run([str(extract), '-provider', 'catalogue', '-from', '-'],
                                   input=source.read_text(), cwd=tmp, capture_output=True, text=True, timeout=30)
            assert stdin.returncode == 0 and json.loads(stdin.stdout) == document, stdin.stderr
            endpoint = f'http://127.0.0.1:{server.server_port}'
            environment = tmp / 'environments.yaml'
            environment.write_text((HERE / 'environments.yaml').read_text().replace('http://127.0.0.1:8765', endpoint))
            home = tmp / 'home'
            base = [binary, '--home', home, '--env-file', environment, '--env', 'demo']
            result = run(base + ['--file', HERE / 'demo.wes'], tmp)
            reports = [json.loads(line.partition(': ')[2]) for line in result.stdout.splitlines()
                       if line.startswith('id') and ': ' in line]
            assert reports[-1]['status'] == 200 and reports[-1]['validation']['state'] == 'validated', result.stdout
            assert reports[-1]['body'] == [{'id': i, 'name': f'Entry {i}'} for i in range(1, 4)], result.stdout
            assert server.requests == ['/items?limit=3'], server.requests
            before = list(server.requests)
            rejected = run([binary, '--home', tmp / 'invalid-home', '--env-file', environment,
                            '--env', 'demo', '--file', HERE / 'invalid.wes'], tmp, expected=1)
            assert 'HTTP001' in rejected.stderr and server.requests == before, rejected.stdout + rejected.stderr
            run([binary, '--home', home, '--command', ':calc { return 1; }'], tmp)
            assert server.requests == before, 'replay dispatched API'
            print('PASS actual OpenAPI -> parser -> descriptor -> environment -> HTTP; schema bounds reject before dispatch, inert replay')

    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


if __name__ == '__main__':
    main()
