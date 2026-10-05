#!/usr/bin/env python3
"""Check the README's OpenAPI -> HTTP response -> table flow with synthetic data."""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile
from threading import Thread
from urllib.parse import parse_qs, urlsplit

from server import BODY, QUERY, make_server

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def prepare(root, binary, extractor, port):
    (root / 'prom.openapi.yaml').write_bytes((HERE / 'prom.openapi.yaml').read_bytes())
    for source in sorted(HERE.glob('*.wes')):
        (root / source.name).write_text(source.read_text().replace(
            ':19092', f':{port}'))

    def run(*flags, data=None):
        result = subprocess.run(
            [str(binary), '--home', str(root / 'home'), *flags], cwd=root,
            input=data, capture_output=True, text=True, timeout=60)
        assert result.returncode == 0, result.stdout + result.stderr
        return result.stdout

    run('--command', '')
    revision = json.loads(run('--api-request', '-', data=json.dumps(
        {'action': 'status'})))['revision']
    run('--api-request', '-', data=json.dumps({
        'action': 'configure', 'expectedRevision': revision,
        'settings': {'localDirectory': str(root / 'home/api-library'),
                     'extractor': str(extractor)},
    }))
    return run


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/wes')
    parser.add_argument('--extractor', type=Path,
                        default=ROOT / 'tools/describe/wes-extract')
    args = parser.parse_args()
    binary, extractor = args.binary.resolve(), args.extractor.resolve()
    server = make_server(0)
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='wes-prom-workspace-') as directory:
            root = Path(directory).resolve()
            run = prepare(root, binary, extractor, server.server_port)
            run('--file', '01-describe.wes')
            run('--file', '02-import.wes')
            assert not server.requests, 'Conversion or import sent a query'
            output = run('--file', '03-query.wes')
            output += run('--file', '04-rates.wes')
            values = [json.loads(line.partition(': ')[2])
                      for line in output.splitlines()
                      if line.startswith('id') and ': ' in line]
            response = next(value for value in values
                            if isinstance(value, dict) and 'validation' in value)
            assert response['status'] == 200, response
            assert response['validation']['state'] == 'validated', response
            assert response['body'] == BODY, response
            expected = [
                {'service': 'orders-api', 'requestsPerSecond': 12.5},
                {'service': 'payments-api', 'requestsPerSecond': 3.25},
                {'service': 'billing-worker', 'requestsPerSecond': 0.8},
            ]
            assert values[-1] == expected, values
            assert len(server.requests) == 1, server.requests
            request = urlsplit(server.requests[0])
            assert request.path == '/api/v1/query', request
            assert parse_qs(request.query) == {'query': [QUERY]}, request
            descriptor = json.loads((root / 'prom.json').read_text())
            assert descriptor['source']['emitted'] == 1, descriptor
            restored = run('--command', ':calc pure { return $rates; } > restored')
            assert json.loads(restored.splitlines()[-1].partition(': ')[2]) == expected, restored
            assert len(server.requests) == 1, 'Reading a kept result repeated HTTP'
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
    print('PASS OpenAPI import, validated HTTP response, derived rates and inert restore')


if __name__ == '__main__':
    main()
