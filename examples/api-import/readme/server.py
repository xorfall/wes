#!/usr/bin/env python3
"""Synthetic public catalogue; loopback only, no external services."""
import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        self.server.requests.append(self.path)
        route = urlsplit(self.path)
        if route.path == '/health':
            self.send_response(204)
            self.end_headers()
            return
        try:
            size = int(parse_qs(route.query)['limit'][0])
        except (KeyError, ValueError):
            size = 0
        if route.path != '/items' or not 1 <= size <= 100:
            self.send_error(400)
            return
        data = json.dumps([{'id': i, 'name': f'Entry {i}'} for i in range(1, size + 1)]).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def make_server(port):
    server = ThreadingHTTPServer(('127.0.0.1', port), Handler)
    server.requests = []
    return server


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=8765)
    args = parser.parse_args()
    with make_server(args.port) as server:
        print(f'Synthetic catalogue: http://127.0.0.1:{server.server_port}', flush=True)
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass
