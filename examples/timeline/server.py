"""Deterministic loopback telemetry, including a missing metric and an exclusive end."""
import json
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

START = int(datetime(2030, 6, 15, 8, tzinfo=timezone.utc).timestamp())
def payload(path):
    if path == '/metrics':
        return {'samples': [{'sequence': i, 'timestamp': START + i * 10, 'value': 10.0 + (i % 11) * 2 + (70 if i % 17 == 0 else 0), 'available': i != 27} for i in range(61)]}
    if path == '/events':
        return {'events': [
            {'id': 'release-a', 'time': '2030-06-15T08:03:15.000000007Z', 'label': 'Release installed', 'detail': 'Synthetic worker revision changed.'},
            {'id': 'release-b', 'time': '2030-06-15T08:07:20Z', 'label': 'Capacity adjusted', 'detail': 'Synthetic pool gained one worker.'},
            {'id': 'end', 'time': '2030-06-15T08:10:00Z', 'label': 'Next interval', 'detail': 'Outside the local half-open window.'},
        ]}
    return None
class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        data = payload(self.path)
        self.server.requests.append(self.path)
        body = json.dumps(data).encode()
        self.send_response(200 if data is not None else 404)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *_):
        pass
def make_server(port=8786):
    server = ThreadingHTTPServer(('127.0.0.1', port), Handler)
    server.requests = []
    return server
if __name__ == '__main__':
    make_server().serve_forever()
