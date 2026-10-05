#!/usr/bin/env python3
"""Check the actual desktop lesson files through the CLI, with isolated data homes."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
from threading import Thread

from fixture import make_server

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def values(output):
    result = []
    for line in output.splitlines():
        if line.startswith('id') and ': ' in line:
            result.append(json.loads(line.partition(': ')[2]))
    return result


def check_views(output):
    rows = values(output)
    table = next(v for v in rows if isinstance(v, list) and len(v) == 3)
    chart = next(v for v in rows if isinstance(v, dict) and v.get('view') == 'line')
    assert all(sorted(row) == ['reading', 'time'] for row in table), table
    assert [r['time'] for r in table] == ['09:00', '09:01', '09:02'], table
    assert [float(r['reading']) for r in table] == [18.25, 19.50, 18.75], table
    assert chart['points'] == [
        {'x': '09:00', 'y': 18.25}, {'x': '09:01', 'y': 19.5}, {'x': '09:02', 'y': 18.75}
    ] and chart['dropped'] == 0, chart
    assert 'http.response' in output and 'recorded' in output, output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/wes')
    parser.add_argument('--extractor', type=Path, default=ROOT / 'tools/describe/wes-extract')
    args = parser.parse_args()
    binary, extractor = args.binary.resolve(), args.extractor.resolve()
    environment = os.environ.copy()
    for key in ('ANTHROPIC_API_KEY', 'OPENAI_API_KEY'):
        environment.pop(key, None)
    server = make_server(0)
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='wes-sensor-walkthrough-') as directory:
            root = Path(directory).resolve()
            endpoint = f'http://127.0.0.1:{server.server_port}'
            for source in HERE.iterdir():
                if source.suffix in ('.wes', '.yaml', '.json'):
                    (root / source.name).write_text(source.read_text().replace('http://127.0.0.1:8771', endpoint))
            generated = subprocess.run(
                [str(extractor), '-provider', 'sensor_demo', '-from', str(HERE / 'openapi.json')],
                text=True, capture_output=True, env=environment, timeout=60)
            assert generated.returncode == 0, generated.stderr
            assert json.loads(generated.stdout) == json.loads((HERE / 'sensor.json').read_text()), 'stale captured descriptor'

            def run(home, *flags, data=None, code=0):
                result = subprocess.run([str(binary), '--home', str(root / home), *flags],
                                        cwd=root, env=environment, input=data,
                                        text=True, capture_output=True, timeout=60)
                assert result.returncode == code, result.stdout + result.stderr
                return result.stdout

            run('desktop', '--command', ':help')
            revision = json.loads(run('desktop', '--api-request', '-', data=json.dumps({'action':'status'})))['revision']
            run('desktop', '--api-request', '-', data=json.dumps({
                'action': 'configure', 'expectedRevision': revision, 'settings': {
                    'localDirectory': str(root / 'desktop/api-library'), 'extractor': str(extractor)}}))
            run('desktop', '--file', str(root / 'describe.wes'))
            managed = ('--env-file', str(root / 'environments.yaml'), '--env', 'sensor_demo')
            run('desktop', *managed, '--file', str(root / 'import.wes'))
            assert server.requests == ['/openapi.json'], server.requests
            run('desktop', '--command', ':env export file:captured.lock.json')
            lock = json.loads((root / 'captured.lock.json').read_text())
            revision = lock['records'][-1]['entry']['after']['sensor_demo']
            selected = ('--env', 'sensor_demo', '--env-revision', revision)
            discovery = run('desktop', *selected, '--activate-env', '--file', str(root / 'discover.wes'))
            assert all(word in discovery for word in ('sensor_demo', 'history', 'sensor', 'Text')), discovery
            assert server.requests == ['/openapi.json'], 'help sent an API request'
            # Restarted managed workspaces require explicit enabling/selection.
            run('desktop', *selected, '--activate-env', '--file', str(root / 'call.wes'))
            assert server.requests == ['/openapi.json', '/history?sensor=LAB1'], server.requests
            check_views(run('desktop', '--file', str(root / 'analyze.wes')))
            assert len(server.requests) == 2, 'views or traces fetched the API again'

            scenario_env = ('--env-file', str(root / 'scenario.environments.yaml'), '--env', 'sensor_demo')
            start = len(server.requests)
            passed = json.loads(run('scenario', *scenario_env, '--test', str(root / 'scenario.yaml')))
            assert passed['status'] == 'passed' and all(s['status'] == 'passed' for s in passed['steps']), passed
            assert passed['steps'][-1]['checks'][0]['status'] == 'passed', passed
            assert server.requests[start:] == ['/history?sensor=LAB1', '/history?sensor=UNKNOWN'], server.requests
            bad = (root / 'scenario.yaml').read_text().replace('$demo_readings.body[1].reading == 19.5', '$demo_readings.body[1].reading == 999')
            (root / 'failing.yaml').write_text(bad)
            start = len(server.requests)
            failed = json.loads(run('failing', *scenario_env, '--test', str(root / 'failing.yaml'), code=1))
            assert failed['status'] == 'failed', failed
            assert [c['status'] for c in failed['steps'][1]['checks']] == ['passed', 'failed'], failed
            assert all(s['status'] == 'skipped' for s in failed['steps'][2:]), failed
            assert server.requests[start:] == ['/history?sensor=LAB1'], server.requests
            server.shutdown()
            server.server_close()
            thread.join()
            before = len(server.requests)
            check_views(run('desktop', '--file', str(root / 'analyze.wes')))
            assert len(server.requests) == before, 'offline restore repeated HTTP'
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
    print('PASS OpenAPI import, discovery, traced readings, table/line, scenario pass/fail, environments and offline views')


if __name__ == '__main__':
    main()
