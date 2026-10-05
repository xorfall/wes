#!/usr/bin/env python3
"""Execute the actual scenario files with an isolated loopback backend and fresh data homes."""
import argparse, json, subprocess, tempfile
from pathlib import Path
from threading import Thread
from http.server import ThreadingHTTPServer
from fixture import Handler
HERE = Path(__file__).resolve().parent

def main():
    parser = argparse.ArgumentParser(); parser.add_argument('--binary', type=Path, default=HERE.parents[1] / 'target/debug/wes')
    binary = parser.parse_args().binary.resolve()
    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = Thread(target=server.serve_forever, daemon=True); thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='wes-api-scenarios-') as directory:
            root = Path(directory)
            for file in HERE.iterdir():
                if file.suffix in ('.yaml', '.json'):
                    (root / file.name).write_text(file.read_text().replace('http://127.0.0.1:8766', f'http://127.0.0.1:{server.server_port}'))
            def run(home, file, env='demo', success=True):
                flags = ['--env-file', str(root / 'environments.yaml'), '--env', env, '--activate-env'] if env else []
                output = subprocess.run([str(binary), '--home', str(root / home), *flags, '--test', str(root / file)], cwd=root.parent, text=True, capture_output=True, timeout=40)
                assert (output.returncode == 0) == success, output.stdout + output.stderr
                return output
            def public(home, file, expected, success=True):
                start = len(Handler.requests)
                output = run(home, file, success=success)
                report = json.loads(output.stdout)
                assert report['schema'] == 1 and (report['status'] == 'passed') == success, report
                actual = Handler.requests[start:]
                assert [method for method, _ in actual] == expected, actual
                assert not Handler.orders, Handler.orders
                return report, actual
            passed, calls = public('public', 'orders.yaml', ['POST', 'GET', 'GET', 'POST', 'DELETE'])
            assert calls[1][1] == calls[-1][1], calls
            assert all(step['status'] == 'passed' for step in passed['steps'] + passed['cleanup']), passed
            assert passed['steps'][3]['checks'][0]['status'] == 'passed', passed
            assert passed['steps'][4]['checks'][0]['status'] == 'passed', passed
            assert passed['steps'][1]['evidence'][0]['environments'][0]['name'] == 'demo', passed
            repeated, _ = public('public', 'orders.yaml', ['POST', 'GET', 'GET', 'POST', 'DELETE'])
            assert repeated['run'] != passed['run'], repeated
            failed, _ = public('public', 'failing.yaml', ['POST', 'DELETE'], success=False)
            assert [c['status'] for c in failed['steps'][1]['checks']] == ['failed', 'passed'], failed
            assert failed['steps'][2]['status'] == 'skipped' and failed['cleanup'][0]['status'] == 'passed', failed
            start = len(Handler.requests)
            private = run('private', 'private.yaml', env='private')
            assert not private.stdout.strip() and 'withheld' in private.stderr, private
            assert [m for m, _ in Handler.requests[start:]] == ['POST', 'GET', 'DELETE']
            assert not Handler.orders
            start = len(Handler.requests)
            direct = json.loads(run('direct', 'direct.yaml', env=None).stdout)
            assert direct['status'] == 'passed' and len(Handler.requests) == start + 2, direct
            # A managed context must not expose the ad-hoc unscoped HTTP provider.
            start = len(Handler.requests)
            refused = json.loads(run('public', 'direct.yaml', success=False).stdout)
            assert refused['status'] == 'failed' and len(Handler.requests) == start, refused
            # The entire file is checked before the first valid step can send its request.
            bad = (root / 'orders.yaml').read_text().replace('$created.body.total == 42', 'call("orders", ["invalid"], {})')
            (root / 'bad.yaml').write_text(bad)
            output = run('bad-home', 'bad.yaml', success=False)
            assert 'TEST001' in output.stderr and not (root / 'bad-home').exists(), output
            assert len(Handler.requests) == start
            server.shutdown(); server.server_close(); thread.join()
            output = subprocess.run([str(binary), '--home', str(root / 'private'), '--command', ':calc { return $created.total; }'], text=True, capture_output=True, timeout=20)
            assert not output.stdout.strip(), 'private API output was restored/exported'
            assert len(Handler.requests) == start
    finally:
        server.shutdown(); server.server_close(); thread.join()
    print('PASS actual API scenarios: ordered references, structured 404/422, failure/cleanup, repeat runs, private omission and managed boundaries')

if __name__ == '__main__': main()
