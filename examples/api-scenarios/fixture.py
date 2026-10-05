#!/usr/bin/env python3
"""Synthetic orders backend; only loopback, no real account or service."""
import argparse, json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

class Handler(BaseHTTPRequestHandler):
    orders = {}
    requests = []
    next_id = 1
    def send(self, status, data):
        body = json.dumps(data).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers(); self.wfile.write(body)
    def do_POST(self):
        Handler.requests.append(('POST', self.path))
        data = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))) or b'{}')
        if self.path != '/orders' or data.get('total', 0) <= 0:
            return self.send(422, {'error': 'invalid total'})
        identifier = Handler.next_id; Handler.next_id += 1
        order = {'id': identifier, 'total': data['total'], 'status': 'created'}
        Handler.orders[identifier] = order
        self.send(201, order)
    def do_GET(self):
        Handler.requests.append(('GET', self.path))
        try: order = Handler.orders[int(self.path.rsplit('/', 1)[1])]
        except (KeyError, ValueError): return self.send(404, {'error': 'missing'})
        self.send(200, order)
    def do_DELETE(self):
        Handler.requests.append(('DELETE', self.path))
        identifier = int(self.path.rsplit('/', 1)[1])
        if Handler.orders.pop(identifier, None) is None: return self.send(404, {'error': 'missing'})
        self.send(200, {'deleted': True})
    def log_message(self, *_): pass

if __name__ == '__main__':
    parser = argparse.ArgumentParser(); parser.add_argument('--port', type=int, default=8766)
    with ThreadingHTTPServer(('127.0.0.1', parser.parse_args().port), Handler) as server:
        print(f'Fixture: http://127.0.0.1:{server.server_port}', flush=True)
        try: server.serve_forever()
        except KeyboardInterrupt: pass
