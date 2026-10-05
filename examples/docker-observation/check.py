#!/usr/bin/env python3
"""Actual example files, synthetic Unix socket by default. --real uses only a created QA container."""
import argparse
import contextlib
import http.server
import json
from pathlib import Path
import socketserver
import subprocess
import tempfile
import threading
import uuid
HERE = Path(__file__).resolve().parent
ID = 'a' * 64
IMAGE = 'sha256:' + 'b' * 64
REQUESTS = []
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_GET(self):
        REQUESTS.append(self.path)
        if self.path == '/version':
            data = {'ApiVersion': '1.47', 'MinAPIVersion': '1.24'}
        elif self.path.startswith('/v1.45/containers/json?'):
            data = [{'Id': ID, 'Names': ['/fixture-api'], 'ImageID': IMAGE, 'State': 'running', 'Created': 1,
                     'Labels': {'com.docker.compose.project': 'wes-observation-qa', 'secret': 'DO_NOT_EXPORT'}}]
        elif self.path == f'/v1.45/containers/{ID}/json':
            data = {'Id': ID, 'Name': '/fixture-api', 'Image': IMAGE, 'Created': '2026-09-24T00:00:00Z',
                    'State': {'Status': 'running', 'Running': True, 'ExitCode': 0, 'OOMKilled': False},
                    'RestartCount': 0, 'Config': {'Env': ['SECRET=DO_NOT_EXPORT']}}
        else:
            self.send_error(404); return
        body = json.dumps(data).encode()
        self.send_response(200); self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)
class Server(socketserver.UnixStreamServer): pass

def command(*args):
    return subprocess.check_output(args, text=True, timeout=60).strip()

@contextlib.contextmanager
def daemon(root, real, socket):
    if not real:
        sock = root / 'docker.sock'
        server = Server(str(sock), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
        try: yield str(sock), ID, "wes-observation-qa"
        finally: server.shutdown(); server.server_close(); thread.join()
    else:
        assert Path(socket).is_socket(), f'Daemon socket unavailable: {socket}'
        host = f'unix://{socket}'
        server = json.loads(command('docker', '--host', host, 'version', '--format', '{{json .Server}}'))
        print('Daemon evidence: ' + json.dumps({k: server.get(k) for k in ['Version', 'ApiVersion', 'MinAPIVersion', 'Os', 'Arch']}))
        tag = 'wes-observation-qa-' + uuid.uuid4().hex
        container = None
        built = False
        try:
            command('docker', '--host', host, 'build', '--network=none', '-t', tag, str(HERE))
            built = True
            container = command('docker', '--host', host, 'create', '--network', 'none', '--label', f'com.docker.compose.project={tag}', '--name', tag, tag)
            # Created but never started; no shell, ports, user container reads or remote image pull.
            yield socket, container, tag
        finally:
            if container: command('docker', '--host', host, 'rm', container)
            if built: command('docker', '--host', host, 'image', 'rm', tag)

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('binary', type=Path)
    parser.add_argument('--real', action='store_true')
    parser.add_argument('--socket', default='/var/run/docker.sock')
    args = parser.parse_args(); binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix='wes-dobs-', dir='/tmp') as folder:
        root = Path(folder)
        with daemon(root, args.real, args.socket) as (socket, container, project):
            recipe = (HERE/'environments.yaml').read_text().replace('SOCKET_PATH', json.dumps(socket))
            (root/'environments.yaml').write_text(recipe)
            results = {}
            for name in ['inventory', 'inspect']:
                program = (HERE/f'{name}.wes').read_text().replace('CONTAINER_ID', container).replace('wes-observation-qa', project)
                (root/f'{name}.wes').write_text(program)
                output = command(str(binary), '--home', str(root/f'home-{name}'), '--env-file', str(root/'environments.yaml'), '--env', 'observe', '--file', str(root/f'{name}.wes'))
                values = [json.loads(line.split(': ', 1)[1]) for line in output.splitlines() if line.startswith('id') and ': ' in line]
                observed = next(v for v in values if isinstance(v, dict) and 'rows' in v)
                assert 'DO_NOT_EXPORT' not in json.dumps(values)
                assert observed['returned'] == 1 and observed['omitted'] == 0 and observed['complete'], observed
                results[name] = observed
            assert any(row['id'] == container for row in results['inventory']['rows'])
            row = results['inspect']['rows'][0]
            assert row['id'] == container and row['restarts'] == 0
            assert 'health' in row and 'Env' not in row
            if not args.real:
                assert len(REQUESTS) == 4, REQUESTS  # one negotiation per fresh process + one actual read
            print('PASS: actual recipe and wes files, typed inventory/inspect, exact identity, omitted sensitive fields; mode=' + ('real daemon' if args.real else 'synthetic'))
if __name__ == '__main__': main()
