#!/usr/bin/env python3
"""Loopback fixture with a Prometheus-shaped response, not a PromQL engine."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from urllib.parse import parse_qs, urlsplit

QUERY = 'sum by (service) (rate(wes_demo_requests_total[5m]))'
RATES = [('orders-api', '12.5'), ('payments-api', '3.25'), ('billing-worker', '0.8')]
BODY = {
    'status': 'success',
    'data': {
        'resultType': 'vector',
        'result': [
            {'metric': {'service': service}, 'value': [1781521200, rate]}
            for service, rate in RATES
        ],
    },
}


def make_server(port=19092):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            self.server.requests.append(self.path)
            request = urlsplit(self.path)
            if request.path != '/api/v1/query':
                status, body = 404, {'error': 'Unknown fixture route'}
            elif parse_qs(request.query).get('query') != [QUERY]:
                status, body = 422, {'error': 'This fixture only accepts the demo query'}
            else:
                status, body = 200, BODY
            data = json.dumps(body).encode()
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    server = ThreadingHTTPServer(('127.0.0.1', port), Handler)
    server.requests = []
    return server


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=19092)
    args = parser.parse_args()
    with make_server(args.port) as server:
        print(f'Synthetic API: http://127.0.0.1:{server.server_port}', flush=True)
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass
