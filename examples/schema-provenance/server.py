#!/usr/bin/env python3
"""Synthetic response-contract examples. No credentials or external services."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json


def make_server(port=8783):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_GET(self):
            self.server.requests.append((self.path, self.headers.get('Authorization')))
            status, body = 200, [{'id': 1}]  # The example's name field is not guaranteed.
            if self.path == '/denied/items':
                status, body = 401, {'error': 'Synthetic authorization failure'}
            elif self.path == '/wrong-status/items':
                status = 201
            elif self.path == '/wrong-shape/items':
                body = [{'id': 'wrong type'}]
            elif self.path != '/ok/items':
                status, body = 404, {'error': 'Synthetic missing route'}
            encoded = json.dumps(body).encode()
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)

    server = ThreadingHTTPServer(('127.0.0.1', port), Handler)
    server.requests = []
    return server


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=8783)
    server = make_server(parser.parse_args().port)
    print(f'Synthetic API at http://127.0.0.1:{server.server_port}', flush=True)
    server.serve_forever()
