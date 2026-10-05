#!/usr/bin/env python3
"""Run the actual OpenAPI -> HTTP -> calc -> typed TimelineGroup example.
Prerequisites: cargo build -p wes; go build -o wes-extract ./cmd/extract in tools/describe.
Run python3 examples/timeline/check.py. Uses an isolated home and loopback server.
Expected: two requests, 60 samples (one gap), two events; exact nanoseconds survive.
For the UI: run server.py, extract openapi.json into telemetry.json, load the
provided environment and execute main.wes. types.yaml declares the optional named contracts.
"""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
from threading import Thread
from server import make_server
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--binary', type=Path, default=ROOT/'target/debug/wes')
    args = parser.parse_args()
    server = make_server(0)
    thread = Thread(target=server.serve_forever, daemon=True); thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='wes-timeline-') as directory:
            tmp = Path(directory)
            extractor = ROOT/'tools/describe/wes-extract'
            result = subprocess.run([str(extractor), '-provider', 'telemetry', '-from', str(HERE/'openapi.json'), '-out', str(tmp/'telemetry.json')], text=True, capture_output=True, timeout=30)
            assert result.returncode == 0, result.stderr
            shutil.copy(HERE/'types.yaml', tmp/'types.yaml')
            env = (HERE/'environments.yaml').read_text().replace('http://127.0.0.1:8786', f'http://127.0.0.1:{server.server_port}')
            (tmp/'environments.yaml').write_text(env)
            result = subprocess.run([str(args.binary.resolve()), '--home', str(tmp/'home'), '--env-file', str(tmp/'environments.yaml'), '--env', 'demo', '--sequential', '--file', str(HERE/'main.wes')], cwd=tmp, text=True, capture_output=True, timeout=60)
            assert result.returncode == 0, result.stdout + result.stderr
            values = [json.loads(line.partition(': ')[2]) for line in result.stdout.splitlines() if line.startswith('id') and ': ' in line]
            group = next(v for v in values if isinstance(v, dict) and 'metric' in v and 'activity' in v)
            metric, events = group['metric'], group['activity']
            assert len(values[-1]['view']['members']['members']) == 2, values[-1]
            samples = metric['series'][0]['samples']
            assert len(samples) == 60 and sum(s['gap'] for s in samples) == 1, samples
            assert len(events['events']) == 2, events
            assert events['events'][0]['at'] == '2030-06-15T08:03:15.000000007Z', events
            assert sorted(server.requests) == ['/events', '/metrics'], server.requests
            nearby = subprocess.run([str(args.binary.resolve()), '--home', str(tmp/'nearby-home'), '--file', str(HERE/'nearby.wes')], cwd=tmp, text=True, capture_output=True, timeout=30)
            assert nearby.returncode == 0, nearby.stdout + nearby.stderr
            records = [json.loads(line.partition(': ')[2]) for line in nearby.stdout.splitlines() if line.startswith('id') and ': ' in line][-1]
            assert [r['label'] for r in records] == ['Retry', 'Started', 'Installed'], records
            assert sorted(server.requests) == ['/events', '/metrics'], server.requests
            encoded = len(json.dumps(group).encode())
            print(f'PASS actual API adaptation: 60 samples, 2 events, exact Instant, {encoded} bytes composite; no implicit refetch')
    finally:
        server.shutdown(); server.server_close(); thread.join(timeout=5)
if __name__ == '__main__':
    main()
