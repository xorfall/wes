#!/usr/bin/env python3
"""Deterministic loopback SSE: healthy -> degraded -> recovered, no real telemetry."""
import argparse
from collections import deque
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import threading
import time


def sample(seq):
    phase = ((seq - 1) // 20) % 3
    duration = (100, 900, 70)[phase] + (seq % 5) * (50 if phase == 1 else 10)
    return {'seq': seq, 'second': f'{seq:04d}', 'route': '/orders' if seq % 2 else '/catalog',
            'durationMs': duration, 'status': 503 if phase == 1 and seq % 3 == 0 else 200}


class Handler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def do_GET(self):
        if self.path != '/events?feed=demo':
            self.send_error(404)
            return
        server = self.server
        with server.changed:
            server.requests.append(self.path)
            server.active += 1
            cursor = server.sequence - len(server.messages)
            server.changed.notify_all()
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream; charset=utf-8')
        self.send_header('Cache-Control', 'no-cache')
        self.send_header('Connection', 'close')
        self.end_headers()
        try:
            self.wfile.write(b': connected\n\n')
            self.wfile.flush()
            while True:
                with server.changed:
                    server.changed.wait_for(lambda: server.sequence > cursor or server.ended, .5)
                    pending = [(n, data) for n, data in server.messages if n > cursor]
                    ended = server.ended
                for n, data in pending:
                    self.wfile.write(f'id: {n}\ndata: {data}\n\n'.encode())
                    cursor = n
                if not pending:
                    self.wfile.write(b': heartbeat\n\n')
                self.wfile.flush()
                if ended:
                    return
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            self.close_connection = True
            with server.changed:
                server.active -= 1
                server.changed.notify_all()

    def log_message(self, *_):
        pass


class DemoServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, port=0):
        super().__init__(('127.0.0.1', port), Handler)
        self.changed = threading.Condition()
        self.messages = deque(maxlen=1000)
        self.sequence = 0
        self.requests = []
        self.active = 0
        self.ended = False
        self.thread = threading.Thread(target=self.serve_forever, daemon=True)
        self.thread.start()

    def publish(self, rows):
        with self.changed:
            for row in rows:
                self.sequence += 1
                self.messages.append((self.sequence, row if isinstance(row, str) else json.dumps(row)))
            self.changed.notify_all()

    def finish(self):
        with self.changed:
            self.ended = True
            self.changed.notify_all()

    def close(self):
        self.finish()
        self.shutdown()
        self.server_close()
        self.thread.join(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=8771)
    parser.add_argument('--interval', type=float, default=.5)
    args = parser.parse_args()
    if args.interval <= 0:
        parser.error('--interval must be positive')
    server = DemoServer(args.port)
    print(f'Synthetic SSE at http://127.0.0.1:{server.server_port}/events?feed=demo', flush=True)
    try:
        seq = 1
        while True:
            server.publish([sample(seq)])
            seq += 1
            time.sleep(args.interval)
    except KeyboardInterrupt:
        pass
    finally:
        server.close()


if __name__ == '__main__':
    main()
