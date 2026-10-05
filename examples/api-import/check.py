#!/usr/bin/env python3
"""Actual Go -> descriptor -> environment -> script -> HTTP integration; isolated synthetic data."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from threading import Thread
from server import make_server
from lesson_support import DEMO_TOKEN

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/wes')
    args = parser.parse_args()
    binary = args.binary.resolve()
    server = make_server(0)
    server.spec = json.loads((HERE / 'openapi.json').read_text())
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='wes-api-import-') as temporary:
            tmp = Path(temporary)
            endpoint = f'http://127.0.0.1:{server.server_port}'
            generated = subprocess.run(['go', 'run', './cmd/extract', '-provider', 'inventory',
                                        '-from', endpoint + '/openapi.json'],
                                       cwd=ROOT / 'tools/describe', capture_output=True, text=True, timeout=60)
            assert generated.returncode == 0, generated.stderr
            document = json.loads(generated.stdout)
            checked_in = json.loads((HERE / 'inventory.json').read_text())
            # Fetching JSON reformats bytes, changing only the source digest.
            for entry in document['source']['provenance']['entries']:
                assert entry['source'] == 'sha256:' + document['source']['sha256']
                entry['source'] = 'sha256:' + checked_in['source']['sha256']
            document['source']['sha256'] = checked_in['source']['sha256']
            assert document == checked_in, 'generated descriptor fixture is stale'
            (tmp / 'inventory.json').write_text(generated.stdout)
            (tmp / 'item-types.yaml').write_bytes((HERE / 'item-types.yaml').read_bytes())
            yaml = (HERE / 'environments.yaml').read_text().replace('http://127.0.0.1:8765', endpoint)
            (tmp / 'environments.yaml').write_text(yaml)
            result = subprocess.run([str(binary), '--home', str(tmp / 'home'), '--env-file',
                                     str(tmp / 'environments.yaml'), '--env', 'demo', '--file', str(HERE / 'demo.wes')],
                                    cwd=tmp, text=True, capture_output=True, timeout=30)
            assert result.returncode == 0, result.stdout + result.stderr
            reports = [json.loads(line.partition(': ')[2]) for line in result.stdout.splitlines() if line.startswith('id') and ': ' in line]
            assert reports[-1] == {'name': 'Demo item', 'hasNote': False, 'removed': True}, result.stdout
            assert [r[0] for r in server.requests] == ['GET', 'GET', 'PUT', 'DELETE'], server.requests
            before = len(server.requests)
            # An out-of-range argument passes structural Int checking, but the adapter refuses it.
            rejected = subprocess.run([str(binary), '--home', str(tmp / 'bad-home'), '--env-file',
                                       str(tmp / 'environments.yaml'), '--env', 'demo', '--file',
                                       str(HERE / 'invalid-argument.wes')],
                                      cwd=tmp, text=True, capture_output=True, timeout=30)
            assert rejected.returncode == 1 and 'HTTP001' in rejected.stderr, rejected.stdout + rejected.stderr
            assert len(server.requests) == before, 'invalid argument was dispatched'
            # Restoring workspace/session data must not contact either API or documentation URL.
            replay = subprocess.run([str(binary), '--home', str(tmp / 'home'), '--command', ':calc { return 1; }'],
                                    cwd=tmp, text=True, capture_output=True, timeout=30)
            assert replay.returncode == 0, replay.stderr
            assert len(server.requests) == before, 'replay performed network I/O'
            print('PASS actual URL -> OpenAPI -> environment -> script: path/query/header/body, 201/204, constraints, inert replay')

            for name, provider in [('auth', 'secureApi'), ('failures', 'failures'), ('maps', 'fx')]:
                generated = subprocess.run([
                    'go', 'run', './cmd/extract', '-provider', provider,
                    '-from', str(HERE / f'{name}.openapi.json'),
                ], cwd=ROOT / 'tools/describe', capture_output=True, text=True, timeout=60)
                assert generated.returncode == 0, generated.stderr
                assert json.loads(generated.stdout) == json.loads((HERE / f'{name}.json').read_text()), name + ' descriptor is stale'
                (tmp / f'{name}.json').write_text(generated.stdout)
                yaml = (HERE / f'{name}.environments.yaml').read_text().replace('http://127.0.0.1:8765', endpoint)
                (tmp / f'{name}.environments.yaml').write_text(yaml)

            before = len(server.requests)
            map_home = tmp / 'maps-home'
            result = subprocess.run([
                str(binary), '--home', str(map_home), '--env-file', str(tmp / 'maps.environments.yaml'),
                '--env', 'demo', '--file', str(HERE / 'maps.wes'),
            ], cwd=tmp, text=True, capture_output=True, timeout=30)
            assert result.returncode == 0, result.stdout + result.stderr
            reports = [json.loads(line.partition(': ')[2]) for line in result.stdout.splitlines() if line.startswith('id') and ': ' in line]
            assert reports[-1] == {'bare': 32.5, 'nested': 32.5, 'usd': 1}, result.stdout
            assert sorted(r[1] for r in server.requests[before:]) == ['/v1/quote', '/v1/rates']
            before = len(server.requests)
            result = subprocess.run([
                str(binary), '--home', str(map_home), '--command', ':calc { return $result.body.rates.TRY; }',
            ], cwd=tmp, text=True, capture_output=True, timeout=30)
            assert result.returncode == 0 and '32.5' in result.stdout, result.stdout + result.stderr
            assert len(server.requests) == before, 'map replay performed network I/O'
            result = subprocess.run([
                str(binary), '--home', str(tmp / 'maps-invalid-home'), '--env-file', str(tmp / 'maps.environments.yaml'),
                '--env', 'demo', '--file', str(HERE / 'maps-invalid.wes'),
            ], cwd=tmp, text=True, capture_output=True, timeout=30)
            assert result.returncode == 0 and '"mismatch"' in result.stdout, result.stdout + result.stderr
            assert [(r[0], r[1]) for r in server.requests[before:]] == [('GET', '/v1/rates-invalid')]
            print('PASS actual extracted bare/nested Decimal maps: mixed JSON numbers, calc key access, inert replay; numeric strings refused')

            for name, code, route in [
                ('limited', 'mismatch', '/v1/limited'),
                ('invalid-response', 'mismatch', '/v1/invalid-response'),
                ('wrong-media', 'mismatch', '/v1/wrong-media'),
            ]:
                before = len(server.requests)
                result = subprocess.run([
                    str(binary), '--home', str(tmp / (name + '-home')), '--env-file',
                    str(tmp / 'failures.environments.yaml'), '--env', 'lesson',
                    '--file', str(HERE / (name + '.wes')),
                ], cwd=tmp, text=True, capture_output=True, timeout=30)
                assert result.returncode == 0 and code in result.stdout, result.stdout + result.stderr
                assert [(r[0], r[1]) for r in server.requests[before:]] == [('GET', route)], 'unexpected retry or dispatch'
            print('PASS actual error scripts: 429 with Retry-After has no automatic retry; response contract/media errors')

            before = len(server.requests)
            rejected = subprocess.run([
                str(binary), '--home', str(tmp / 'no-credential-home'), '--env-file',
                str(tmp / 'auth.environments.yaml'), '--env', 'lesson', '--file', str(HERE / 'auth.wes'),
            ], cwd=tmp, text=True, capture_output=True, timeout=30)
            assert rejected.returncode == 1, rejected.stdout + rejected.stderr
            assert len(server.requests) == before, 'ungranted/missing credential reached API'
            # Invoke the actual documented helper, not a checker-only copy of credential delivery.
            result = subprocess.run([
                sys.executable, str(HERE / 'run-auth.py'), '--home', str(tmp / 'auth-home'),
                '--binary', str(binary), '--env-file', str(tmp / 'auth.environments.yaml'), '--demo-token',
            ], cwd=tmp, text=True, capture_output=True, timeout=40)
            assert result.returncode == 0, result.stdout + result.stderr
            assert [(r[0], r[1]) for r in server.requests[before:]] == [('GET', '/v1/secure')]
            assert DEMO_TOKEN not in result.stdout + result.stderr
            assert 'private-fixture-result' not in result.stdout + result.stderr, 'private response leaked to stdout'
            assert DEMO_TOKEN not in repr(server.requests), 'fixture logs retained credential'
            print('PASS actual auth helper: stdin material + exact provider grant; private output and redacted fixture logs')

            extract = ['go', 'run', './cmd/extract', '-provider', 'partial', '-from', str(HERE / 'partial.openapi.json')]
            output = tmp / 'partial.json'
            result = subprocess.run(extract + ['-out', str(output)], cwd=ROOT / 'tools/describe',
                                    text=True, capture_output=True, timeout=60)
            assert result.returncode != 0 and not output.exists() and not result.stdout, result.stdout + result.stderr
            result = subprocess.run(extract + ['-allow-partial', '-out', str(output)], cwd=ROOT / 'tools/describe',
                                    text=True, capture_output=True, timeout=60)
            assert result.returncode == 0, result.stderr
            raw = output.read_bytes()
            partial = json.loads(raw)
            assert {k: partial['source'][k] for k in ['discovered', 'emitted', 'skipped']} == {'discovered': 2, 'emitted': 1, 'skipped': 1}
            assert [op['path'] for op in partial['operations']] == [['health']]
            assert partial['diagnostics'], 'skipped union must have diagnostics'
            repeated = subprocess.run(extract + ['-allow-partial', '-out', str(output)], cwd=ROOT / 'tools/describe',
                                      text=True, capture_output=True, timeout=60)
            assert repeated.returncode != 0 and output.read_bytes() == raw, 'extractor overwrote reviewed output'
            print('PASS partial extraction: default refuses output; explicit subset has evidence; existing output never overwritten')
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


if __name__ == '__main__':
    main()
