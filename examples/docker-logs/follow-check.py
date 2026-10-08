#!/usr/bin/env python3
"""Exercise actual follow/summary/cancel files through an isolated running engine."""
import argparse
import contextlib
import http.server
import json
from pathlib import Path
import socketserver
import struct
import sys
import tempfile
import threading
import time
from urllib.parse import urlsplit, parse_qs
from check import fixtures, ID, HERE
sys.path.insert(0, str(HERE.parent / 'live-service-monitor'))
from support import Client


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        with self.server.changed:
            self.server.requests.append(self.path)
        path = urlsplit(self.path)
        if path.path == '/version':
            body = json.dumps({'ApiVersion': '1.47', 'MinAPIVersion': '1.24'}).encode()
        elif path.path == f'/v1.45/containers/{ID}/json':
            body = json.dumps({'Id': ID, 'Config': {'Tty': self.server.tty}}).encode()
        elif path.path == f'/v1.45/containers/{ID}/logs':
            assert parse_qs(path.query)['follow'] == ['true']
            self.send_response(200)
            self.send_header('Connection', 'close')
            self.end_headers()
            with self.server.changed:
                self.server.active += 1
                self.server.changed.notify_all()
            try:
                for n in range(1, 701):
                    payload = f'2026-09-24T00:00:00Z {"ERROR" if n % 25 == 0 else "INFO"} synthetic_live {n}\n'.encode()
                    self.wfile.write(payload if self.server.tty else struct.pack('>BxxxI', 1, len(payload)) + payload)
                self.wfile.flush()
                # Silent from the application's perspective; empty frames detect physical closure.
                # In TTY mode use a socket peek with timeout instead of fabricating text/lines.
                if self.server.tty:
                    self.connection.settimeout(10)
                    self.connection.recv(1)
                else:
                    while True:
                        time.sleep(.05)
                        self.wfile.write(struct.pack('>BxxxI', 1, 0))
                        self.wfile.flush()
            except (OSError, TimeoutError):
                pass
            finally:
                with self.server.changed:
                    self.server.active -= 1
                    self.server.changed.notify_all()
            return
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class Server(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True


@contextlib.contextmanager
def synthetic(root):
    sock = root / 'd.sock'
    server = Server(str(sock), Handler)
    server.changed, server.requests, server.active = threading.Condition(), [], 0
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server, [(str(sock), ID, tty, lambda tty=tty: setattr(server, 'tty', tty)) for tty in [False, True]]
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('binary', type=Path)
    parser.add_argument('--real', action='store_true')
    parser.add_argument('--socket', default='/var/run/docker.sock')
    args = parser.parse_args()
    with contextlib.ExitStack() as stack:
        root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='wes-dfollow-', dir='/tmp')))
        if args.real:
            server = None
            cases = stack.enter_context(fixtures(root, True, args.socket, follow=True))
        else:
            server, cases = stack.enter_context(synthetic(root))
        for socket, container, tty, prepare in cases:
            prepare()
            case = root / str(tty)
            case.mkdir()
            for name in ['environments.yaml', 'follow.wes', 'cancel.wes']:
                (case / name).write_text((HERE / name).read_text().replace('SOCKET_PATH', json.dumps(socket)).replace('CONTAINER_ID', container))
            client = Client(args.binary.resolve(), case, environment='observe')
            try:
                client.submit(':env plan file:environments.yaml > plan')
                client.submit(':env apply $plan')
                client.wait(lambda: client.context)
                client.submit(':env use "observe"')
                client.submit(':workspace policy mode:reactive')
                client.file('follow.wes')
                value = client.value('live_logs', lambda rows: len(rows) == 500 and rows[-1]['sequence'] > 520)
                rows = value['data']
                assert rows[0]['sequence'] > 1
                assert all(row['container'] == container for row in rows)
                assert {row['stream'] for row in rows} == ({'tty'} if tty else {'stdout'})
                summary = client.value('live_summary', lambda s: s['lines'] == 500 and s['last'] > 520)['data']
                assert 0 <= summary['errors'] <= summary['analyzed'] == 20 and summary['clipped'] == 0
                if server:
                    assert len(server.requests) == (3 if not tty else 6)
                    assert summary['last'] == 700 and summary['errors'] == 1
                client.file('cancel.wes')
                if server:
                    with server.changed:
                        assert server.changed.wait_for(lambda: server.active == 0, 5)
                for name in ['live_logs', 'live_summary']:
                    client.wait(lambda: client.ready.get(client.names[name], {}).get('event') == 'evidence' and client.ready[client.names[name]].get('kind') == 'stopped_stream')
                stopped_rows = client.value('live_logs')['data']
                stopped_summary = client.value('live_summary')['data']
                assert len(stopped_rows) == 500 and stopped_summary['lines'] == 500
                covered = [row for row in stopped_rows if stopped_summary['last'] - 20 < row['sequence'] <= stopped_summary['last']]
                assert len(covered) == 20
                assert stopped_summary['errors'] == sum('ERROR' in row['text'] for row in covered)
                print(f'PASS: actual follow/calc/cancel files, rolling window and stopped values; tty={tty}; mode={"real" if args.real else "synthetic"}')
            finally:
                client.close()


if __name__ == '__main__':
    main()
