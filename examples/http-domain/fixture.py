#!/usr/bin/env python3
"""Synthetic, loopback-only HTTP analysis fixture."""
import argparse, json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

class Handler(BaseHTTPRequestHandler):
    requests = []
    def do_GET(self):
        Handler.requests.append(self.path)
        status = {'/missing': 404, '/invalid': 422}.get(self.path, 200)
        body = b'not valid json' if self.path == '/bad-json' else json.dumps({'reading': 42}).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *_): pass

if __name__ == '__main__':
    parser = argparse.ArgumentParser(); parser.add_argument('--port', type=int, default=8767)
    with ThreadingHTTPServer(('127.0.0.1', parser.parse_args().port), Handler) as server:
        print(f'Fixture: http://127.0.0.1:{server.server_port}', flush=True)
        try: server.serve_forever()
        except KeyboardInterrupt: pass
