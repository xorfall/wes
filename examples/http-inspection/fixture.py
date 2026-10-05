#!/usr/bin/env python3
"""Synthetic loopback fixture; no external sensor service or credentials."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import argparse
HISTORY = b'[ {"time":"09:00","reading":18.25}, {"time":"09:01","reading":19.50}, {"time":"09:02","reading":18.75} ]'
class Handler(BaseHTTPRequestHandler):
    requests = 0
    def do_GET(self):
        Handler.requests += 1
        status, body = (200, b'{"reading":42}') if self.path == '/sample' else (404, b'{"error":"fixture missing"}')
        if self.path == '/history': status, body = 200, HISTORY
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *_): pass
if __name__ == '__main__':
    parser = argparse.ArgumentParser(); parser.add_argument('--port', type=int, default=8765)
    with ThreadingHTTPServer(('127.0.0.1', parser.parse_args().port), Handler) as server:
        print(f'Fixture: http://127.0.0.1:{server.server_port}', flush=True)
        try: server.serve_forever()
        except KeyboardInterrupt: pass
