#!/usr/bin/env python3
"""Execute the actual URL import example against a synthetic source, then replay offline."""
import argparse
import json
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import subprocess
import tempfile
from threading import Thread

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


class Handler(SimpleHTTPRequestHandler):
    requests = 0

    def do_GET(self):
        Handler.requests += 1
        super().do_GET()

    def log_message(self, *_):
        pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/wes')
    binary = parser.parse_args().binary.resolve()
    server = ThreadingHTTPServer(('127.0.0.1', 0), partial(
        Handler, directory=str(ROOT / 'tests/fixtures')))
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='wes-spec-url-') as directory:
            root = Path(directory)
            endpoint = f'http://127.0.0.1:{server.server_port}'
            for name in ['import.wes', 'environments.yaml']:
                (root / name).write_text((HERE / name).read_text().replace(
                    'http://127.0.0.1:8765', endpoint))

            def run(home, *args):
                result = subprocess.run([str(binary), '--home', str(root / home), *args],
                                        cwd=root, text=True, capture_output=True, timeout=40)
                assert result.returncode == 0, result.stdout + result.stderr
                if home == 'default':
                    assert 'catalog' in result.stdout, result.stdout

            run('default', '--file', str(root / 'import.wes'))
            run('managed', '--env-file', str(root / 'environments.yaml'),
                '--env', 'demo', '--command', ':env export file:captured.lock.json')
            captured = json.loads((root / 'captured.lock.json').read_text())
            assert 'catalog' in json.dumps(captured)
            assert endpoint + '/catalog.provider.json' in json.dumps(captured)
            assert Handler.requests == 2, Handler.requests
            server.shutdown()
            server.server_close()
            thread.join()
            run('default', '--command', ':inspect catalog get')
            run('managed', '--command', ':env export file:restored.lock.json')
            assert json.loads((root / 'restored.lock.json').read_text()) == captured
            assert Handler.requests == 2, 'replay performed network I/O'
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
    print('PASS spec URL commands, environment recipe and offline replay')


if __name__ == '__main__':
    main()
