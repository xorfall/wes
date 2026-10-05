#!/usr/bin/env python3
"""Read-only synthetic sensor API on loopback; no external services or credentials."""
import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

HERE = Path(__file__).resolve().parent
READINGS = [
    {"time": "09:00", "reading": 18.25},
    {"time": "09:01", "reading": 19.50},
    {"time": "09:02", "reading": 18.75},
]


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        self.server.requests.append(self.path)
        route = urlsplit(self.path)
        if route.path == '/openapi.json':
            status, body = 200, (HERE / 'openapi.json').read_bytes()
        elif route.path == '/history':
            sensor = parse_qs(route.query).get('sensor')
            if sensor == ['LAB1']:
                status, body = 200, json.dumps(READINGS).encode()
            elif sensor:
                status, body = 404, b'{"error":"unknown synthetic sensor"}'
            else:
                status, body = 400, b'{"error":"sensor is required"}'
        else:
            status, body = 404, b'{"error":"unknown fixture route"}'
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def make_server(port=8771):
    server = ThreadingHTTPServer(('127.0.0.1', port), Handler)
    server.requests = []
    return server


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=8771)
    server = make_server(parser.parse_args().port)
    print(f'Sensor demo ready: http://127.0.0.1:{server.server_port}/openapi.json', flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
