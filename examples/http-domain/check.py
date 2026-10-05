#!/usr/bin/env python3
"""Run actual checked-in files with isolated homes and a synthetic loopback service."""
import argparse, json, subprocess, tempfile
from pathlib import Path
from threading import Thread
from http.server import ThreadingHTTPServer
from fixture import Handler
HERE = Path(__file__).resolve().parent

def values(output):
    result = []
    for line in output.splitlines():
        _, separator, data = line.partition(': ')
        if separator:
            try: result.append(json.loads(data))
            except json.JSONDecodeError: pass
    return result

def main():
    parser = argparse.ArgumentParser(); parser.add_argument('--binary', type=Path, default=HERE.parents[1] / 'target/debug/wes')
    binary = parser.parse_args().binary.resolve()
    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = Thread(target=server.serve_forever, daemon=True); thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='wes-http-domain-') as directory:
            root = Path(directory)
            for file in HERE.iterdir():
                if file.suffix in ('.yaml', '.json', '.wes'):
                    (root / file.name).write_text(file.read_text().replace('http://127.0.0.1:8767', f'http://127.0.0.1:{server.server_port}'))
            def run(home, *args, success=True):
                result = subprocess.run([str(binary), '--home', str(root / home), *args], cwd=root, text=True, capture_output=True, timeout=40)
                assert (result.returncode == 0) == success, result.stdout + result.stderr
                return result
            def scenario(home, file, env=None):
                flags = ['--env-file', 'environments.yaml', '--env', env, '--activate-env'] if env else []
                return run(home, *flags, '--test', file)
            public = json.loads(scenario('public', 'analysis.yaml', 'demo').stdout)
            assert public['status'] == 'passed', public
            assert Handler.requests == ['/sample', '/missing', '/invalid', '/bad-json'], Handler.requests
            private = scenario('private', 'private.yaml', 'private')
            assert not private.stdout.strip() and 'withheld' in private.stderr, private
            assert json.loads(scenario('direct', 'direct.yaml').stdout)['status'] == 'passed'
            assert len(Handler.requests) == 6
            server.shutdown(); server.server_close(); thread.join()
            # Offline read/analysis in a managed, deactivated workspace must send no requests.
            offline = values(run('public', '--file', 'offline.wes').stdout)
            analysis = next(v for v in offline if isinstance(v, dict) and v.get('analyzer') == 'wes.http')
            assert analysis['statuses'][0]['code'] == 200 and analysis['executionState'] == 'completed', analysis
            assert any(isinstance(v, list) for v in offline), offline
            catalogue_output = run('public', '--file', 'catalogue.wes').stdout
            assert 'calc' in catalogue_output
            catalogues = values(catalogue_output)
            assert any(isinstance(v, dict) and v.get('registration') == 'unassigned' for v in catalogues), catalogues
            assert any(isinstance(v, dict) and v.get('known') is False for v in catalogues), catalogues
            assert sum(isinstance(v, list) for v in catalogues) >= 2
            refused = run('public', '--command', 'http request url:"http://127.0.0.1:1"', success=False)
            assert not refused.stdout.strip()
            for name in ('analysis', 'definition'):
                assert not run('private', '--command', f':calc {{ return ${name}; }}').stdout.strip()
            assert len(Handler.requests) == 6
    finally:
        server.shutdown(); server.server_close(); thread.join()
    print('PASS HTTP domain examples: definitions, managed analysis, 404/422/decode distinction, typed tables, private omission, offline replay and exact dispatch counts')

if __name__ == '__main__': main()
