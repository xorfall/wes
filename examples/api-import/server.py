#!/usr/bin/env python3
"""Synthetic loopback API only. No files, credentials or external services are accessed."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from urllib.parse import urlsplit, parse_qs
from lesson_support import DEMO_TOKEN

ITEM = {"id": "a/b", "name": "Demo item", "note": None,
        "labels": {"team": "demo"}, "nested": {"enabled": True}}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def send(self, status, data=None, *, media='application/json', headers=None):
        body = b'' if data is None else json.dumps(data).encode()
        self.send_response(status)
        if data is not None:
            self.send_header('Content-Type', media)
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def record_request(self):
        headers = {k: '<redacted>' if k.lower() == 'authorization' else v
                   for k, v in self.headers.items()}
        self.server.requests.append((self.command, self.path, headers))

    def do_GET(self):
        self.record_request()
        route = urlsplit(self.path)
        if route.path == '/openapi.json':
            self.send(200, self.server.spec)
            return
        if route.path == '/v1/secure':
            if self.headers.get('Authorization') != 'Bearer ' + DEMO_TOKEN:
                self.send(401, {'error': 'synthetic credential required'})
            else:
                self.send(200, {'message': 'private-fixture-result'})
            return
        if route.path == '/v1/limited':
            self.send(429, {'error': 'synthetic rate limit'}, headers={'Retry-After': '2'})
            return
        if route.path == '/v1/invalid-response':
            self.send(200, {'ready': 'not-a-boolean'})
            return
        if route.path == '/v1/wrong-media':
            self.send(200, {'ready': True}, media='text/html')
            return
        if route.path in ['/v1/rates', '/v1/quote', '/v1/rates-invalid']:
            rates = {'USD': 1, 'TRY': 32.5}  # Deliberately mix integer and fractional JSON tokens.
            if route.path == '/v1/rates-invalid':
                rates['USD'] = '1.0'  # Numeric strings must still fail the declared Decimal.
            self.send(200, {'base': 'USD', 'rates': rates} if route.path == '/v1/quote' else rates)
            return
        if route.path != '/v1/items/a%2Fb':
            self.send(404, {'error': 'unknown fixture route'})
            return
        assert parse_qs(route.query) == {'tag': ['red', 'blue'], 'limit': ['2']}
        assert self.headers['X-Request-Id'] == 'demo'
        assert not self.headers.get('Content-Length')
        self.send(200, ITEM)

    def do_PUT(self):
        self.record_request()
        assert self.path == '/v1/items/a%2Fb'
        size = int(self.headers['Content-Length'])
        assert 0 < size < 4096
        assert self.headers['Content-Type'] == 'application/json'
        assert json.loads(self.rfile.read(size)) == ITEM  # whole object, not {body: ...}
        self.send(201, ITEM)

    def do_DELETE(self):
        self.record_request()
        assert self.path == '/v1/items/a%2Fb'
        assert not self.headers.get('Content-Length')
        self.send(204)


def make_server(port=8765):
    server = ThreadingHTTPServer(('127.0.0.1', port), Handler)
    server.requests = []
    return server


if __name__ == '__main__':
    from pathlib import Path
    server = make_server()
    server.spec = json.loads(Path(__file__).with_name('openapi.json').read_text())
    print('Synthetic API/documentation at http://127.0.0.1:8765 — Ctrl-C stops it.', flush=True)
    server.serve_forever()
