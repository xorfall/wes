#!/usr/bin/env python3
"""Actual finite-log/calc files; synthetic by default, real mode creates only owned fixtures."""
import argparse
import contextlib
import http.server
import json
import os
from pathlib import Path
import shutil
import socketserver
import struct
import subprocess
import tempfile
import threading
import uuid

HERE = Path(__file__).resolve().parent
ID = 'a' * 64
LINES = [(1, 'INFO ready — sentetik'), (1, 'ERROR synthetic_stdout'),
         (2, 'ERROR synthetic_stderr'), (1, 'WARN fixture'), (1, 'INFO done')]
REQUESTS = []


def command(*args, **kwargs):
    return subprocess.check_output(args, text=True, timeout=180, **kwargs).strip()


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        REQUESTS.append(self.path)
        if self.path == '/version':
            body = json.dumps({'ApiVersion': '1.47', 'MinAPIVersion': '1.24'}).encode()
        elif self.path == f'/v1.45/containers/{ID}/json':
            body = json.dumps({'Id': ID, 'Config': {'Tty': self.server.tty, 'Env': ['DO_NOT_EXPORT']}}).encode()
        elif self.path.startswith(f'/v1.45/containers/{ID}/logs?'):
            body = b''
            for channel, text in LINES:
                payload = ('2026-09-24T00:00:00.123456789Z ' + text + '\n').encode()
                body += payload if self.server.tty else struct.pack('>BxxxI', channel, len(payload)) + payload
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class Server(socketserver.UnixStreamServer):
    pass


@contextlib.contextmanager
def fixtures(root, real, socket, follow=False):
    if not real:
        sock = root / 'docker.sock'
        server = Server(str(sock), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield [(str(sock), ID, tty, lambda tty=tty: setattr(server, 'tty', tty)) for tty in [False, True]]
        finally:
            server.shutdown()
            server.server_close()
            thread.join()
        return
    assert Path(socket).is_socket(), f'Daemon socket unavailable: {socket}'
    docker = ['docker', '--host', f'unix://{socket}']
    version = json.loads(command(*docker, 'version', '--format', '{{json .Server}}'))
    print('Daemon evidence: ' + json.dumps({k: version.get(k) for k in ['Version', 'ApiVersion', 'MinAPIVersion', 'Os', 'Arch']}))
    assert version['Os'] == 'linux' and version['Arch'] in ['amd64', 'arm64']
    context = root / 'build'
    context.mkdir()
    shutil.copyfile(HERE / 'Dockerfile', context / 'Dockerfile')
    env = dict(os.environ, CGO_ENABLED='0', GOOS='linux', GOARCH=version['Arch'])
    command('go', 'build', '-trimpath', '-o', str(context / 'fixture'), str(HERE / 'fixture.go'), env=env)
    tag = 'wes-logs-qa-' + uuid.uuid4().hex
    containers = []
    built = False
    try:
        command(*docker, 'build', '--network=none', '-t', tag, str(context))
        built = True
        for tty in [False, True]:
            container = command(*docker, 'create', '--network=none', '--log-driver=json-file',
                                '--label', f'wes.qa={tag}', '--name', tag + ('-tty' if tty else '-pipes'),
                                *(['--tty'] if tty else []), tag, *(['--follow'] if follow else []))
            containers.append((socket, container, tty, lambda: None))
            command(*docker, 'start', container)
            if not follow:
                assert command(*docker, 'wait', container) == '0'
        yield containers
    finally:
        for _, container, _, _ in containers:
            command(*docker, 'rm', '-f', container)
        if built:
            command(*docker, 'image', 'rm', tag)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('binary', type=Path)
    parser.add_argument('--real', action='store_true')
    parser.add_argument('--socket', default='/var/run/docker.sock')
    args = parser.parse_args()
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix='wes-dlogs-', dir='/tmp') as folder:
        root = Path(folder)
        with fixtures(root, args.real, args.socket) as cases:
            for socket, container, tty, prepare in cases:
                prepare()
                recipe = (HERE / 'environments.yaml').read_text().replace('SOCKET_PATH', json.dumps(socket))
                (root / 'environments.yaml').write_text(recipe)
                program = (HERE / 'analyze.wes').read_text().replace('CONTAINER_ID', container)
                (root / 'analyze.wes').write_text(program)
                output = command(str(binary), '--home', str(root / f'home-{tty}'),
                                 '--env-file', str(root / 'environments.yaml'), '--env', 'observe',
                                 '--file', str(root / 'analyze.wes'))
                values = [json.loads(line.split(': ', 1)[1]) for line in output.splitlines()
                          if line.startswith('id') and ': ' in line]
                logs = next(v for v in values if isinstance(v, dict) and 'rows' in v)
                summary = next(v for v in values if isinstance(v, dict) and 'errors' in v)
                assert logs['returned'] == 5 and not logs['truncated'], logs
                assert logs['container'] == container and logs['requested_tail'] == 200
                assert all(row['timestamp_ns'] is not None for row in logs['rows']), logs
                assert not any(row['partial'] or row['lossy'] for row in logs['rows']), logs
                assert {r['stream'] for r in logs['rows']} == ({'tty'} if tty else {'stdout', 'stderr'})
                assert sorted(r['text'] for r in logs['rows']) == sorted(text for _, text in LINES)
                assert summary['errors'] == 2 and summary['lines'] == 5, summary
                assert summary['stderr_lines'] == (0 if tty else 1), summary
                assert sorted(item['preview'] for item in summary['examples']) == ['ERROR synthetic_stderr', 'ERROR synthetic_stdout']
                assert not any(item['shortened'] for item in summary['examples'])
                assert len(json.dumps(summary)) < 1500
                assert not summary['truncated'] and summary['truncation'] == 'none', summary
                assert 'DO_NOT_EXPORT' not in json.dumps(values)
                print(f'PASS: actual recipe/logs/calc; tty={tty}; small summary={len(json.dumps(summary))} JSON characters')
            if not args.real:
                assert len(REQUESTS) == 6, REQUESTS  # two fresh clients: each version + inspect + logs


if __name__ == '__main__':
    main()
