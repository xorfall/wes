#!/usr/bin/env python3
"""Verify exact OpenAPI retrieval, HTML rejection, explicit import and inert replay."""
import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import subprocess
import sys
import tempfile
from threading import Thread

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
BINARY = ROOT / 'target/debug/wes'
EXTRACTOR = ROOT / 'tools/describe/wes-extract'


class Handler(BaseHTTPRequestHandler):
    requests = 0

    def do_GET(self):
        Handler.requests += 1
        files = {'/native/inventory.json': ROOT / 'examples/api-import/openapi.json',
                 '/prose.html': HERE / 'prose.html', '/limits.html': HERE / 'limits.html'}
        if self.path == '/docs/overview':
            body = (HERE / 'overview.html').read_bytes()
        elif self.path in files:
            body = files[self.path].read_bytes()
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


def main():
    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='wes-describe-command-') as directory:
            root = Path(directory).resolve()
            endpoint = f'http://127.0.0.1:{server.server_port}'
            def run(home, *args, data=None, expected=0):
                result = subprocess.run([str(BINARY), '--home', str(root / home), *args],
                                        cwd=root, input=data, text=True,
                                        capture_output=True, timeout=60)
                assert result.returncode == expected, result.stdout + result.stderr
                return result

            def configure(home, executable):
                run(home, '--command', ':help')
                revision = json.loads(run(home, '--api-request', '-', data=json.dumps({'action':'status'})).stdout)['revision']
                run(home, '--api-request', '-', data=json.dumps({
                    'action': 'configure', 'expectedRevision': revision, 'settings': {
                        'localDirectory': str(root / home / 'api-library'), 'extractor': str(executable)}}))

            configure('structured', EXTRACTOR)
            describe_script = (HERE / 'describe.wes').read_text().replace('http://127.0.0.1:8765', endpoint)
            (root / 'describe.wes').write_text(describe_script)
            run('structured', '--file', str(root / 'describe.wes'))
            assert (root / 'inventory.generated.json').exists()
            script = (HERE / 'document-import.wes').read_text().replace('http://127.0.0.1:8765', endpoint)
            (root / 'document-import.wes').write_text(script)
            assert 'inventory' in run('structured', '--file', str(root / 'document-import.wes')).stdout

            assert Handler.requests == 1, 'import refetched the source or crawled documentation'
            run('structured', '--command', f':describe url:"{endpoint}/docs/overview" provider:html', expected=1)
            assert Handler.requests == 2, 'HTML rejection crawled linked documents'
            expected_requests = Handler.requests
            server.shutdown()
            server.server_close()
            thread.join()
            # Saved descriptor import and restored bindings are independent of the source URL.
            run('structured', '--file', str(root / 'document-import.wes'))
            assert Handler.requests == expected_requests
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
    print('PASS exact OpenAPI import, HTML rejection without crawling, explicit file reuse and offline replay')


if __name__ == '__main__':
    main()
