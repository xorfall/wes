#!/usr/bin/env python3
"""Check actual example files, isolated public/private homes and offline trace restoration."""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile
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
        with tempfile.TemporaryDirectory(prefix='wes-inspection-') as directory:
            root = Path(directory)
            for name in ['sensor.json', 'environments.yaml', 'call.wes', 'inspect.wes', 'direct.wes', 'history.wes', 'analyze.wes', 'direct-history.wes', 'decode.wes']:
                (root / name).write_text((HERE / name).read_text().replace('http://127.0.0.1:8765', f'http://127.0.0.1:{server.server_port}'))
            def run(home, *args, success=True):
                result = subprocess.run([str(binary), '--home', str(root / home), *args], cwd=root, text=True, capture_output=True, timeout=40)
                assert (result.returncode == 0) == success, result.stdout + result.stderr
                return result.stdout
            run('public', '--env-file', 'environments.yaml', '--env', 'demo', '--file', 'call.wes')
            run('private', '--env-file', 'environments.yaml', '--env', 'private', '--file', 'call.wes')
            direct = run('direct', '--file', 'direct.wes'); assert '404' in direct, direct
            expected = [{"x":"09:00","y":18.25}, {"x":"09:01","y":19.50}, {"x":"09:02","y":18.75}]
            def chart(output):
                values = []
                for line in output.splitlines():
                    _, separator, value = line.partition(': ')
                    if separator:
                        try: values.append(json.loads(value))
                        except json.JSONDecodeError: pass
                tables = [v for v in values if isinstance(v, list) and len(v) == 3]
                assert tables, output
                assert all(sorted(row) == ['reading', 'time'] for row in tables[0]), tables
                assert [row['time'] for row in tables[0]] == ['09:00', '09:01', '09:02'], tables
                assert [float(row['reading']) for row in tables[0]] == [18.25, 19.50, 18.75], tables
                plots = [v for v in values if isinstance(v, dict) and v.get('view') == 'line']
                assert len(plots) == 1, output
                assert plots[0]['points'] == expected and plots[0]['dropped'] == 0, plots
                assert plots[0]['x'] == 'time' and plots[0]['y'] == 'reading', plots
                return plots[0]
            public_chart = chart(run('public', '--env-file', 'environments.yaml', '--env', 'demo', '--activate-env', '--file', 'history.wes'))
            private_output = run('private', '--env-file', 'environments.yaml', '--env', 'private', '--activate-env', '--file', 'history.wes')
            assert not private_output.strip(), 'private source or derived views leaked to batch stdout'
            run('direct', '--file', 'direct-history.wes')
            assert Handler.requests == 6, Handler.requests
            server.shutdown(); server.server_close(); thread.join()
            inspection = run('public', '--file', 'inspect.wes')
            assert 'recorded' in inspection and 'http.response' in inspection and '42' in inspection, inspection
            run('private', '--command', ':read trace:$sample', success=False)
            raw = run('direct', '--command', ':read trace:$response'); assert '404' in raw and 'completed' in raw, raw
            assert chart(run('public', '--file', 'analyze.wes')) == public_chart
            assert chart(run('direct', '--file', 'decode.wes')) == public_chart
            assert not run('private', '--command', ':calc { return $reading_chart.points; }').strip(), 'private derived content became available on restart'
            assert Handler.requests == 6, 'inspection, decoding or chart rendering performed network I/O'
            run('public', '--command', '@trace(http) http request url:"http://127.0.0.1:1"', success=False)
    finally:
        server.shutdown(); server.server_close(); thread.join()
    print('PASS actual HTTP inspection examples, environment isolation, private omission and offline replay, UTF-8/JSON decoding and sensor charts')
if __name__ == '__main__': main()
