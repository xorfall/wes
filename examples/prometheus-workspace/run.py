#!/usr/bin/env python3
"""Open the synthetic Prometheus workspace in the real GUI, with an isolated home."""
import argparse
import json
from pathlib import Path
import selectors
import signal
import subprocess
import tempfile
from threading import Thread
import urllib.request
import uuid

from check import ROOT, prepare
from server import make_server


def seed(url, root):
    """Submit each finite step to the running engine; never retry external work."""
    with urllib.request.urlopen(url + '/events', timeout=20) as stream:
        def event():
            while True:
                line = stream.readline()
                if not line:
                    raise RuntimeError('Engine event stream ended')
                if line.startswith(b'data:'):
                    return json.loads(line[5:])

        generation = None
        while generation is None:
            frame = event()
            if frame['event'] == 'session':
                generation = frame['generation']

        for name in ('01-describe.wes', '02-import.wes', '03-query.wes', '04-rates.wes'):
            cell = str(uuid.uuid4())
            request = urllib.request.Request(
                url + '/submit', json.dumps({
                    'request': 'submit', 'cell': cell,
                    'text': (root / name).read_text(),
                    'client': 'prometheus-workspace-example', 'environments': None,
                }).encode(),
                {'Content-Type': 'application/json', 'X-Wes-Session': generation})
            with urllib.request.urlopen(request, timeout=20) as response:
                response.read()
            planned, ready = None, set()
            while planned is None or not set(planned['nodes']).issubset(ready):
                frame = event()
                if frame['event'] == 'failed':
                    raise RuntimeError(str(frame))
                if frame['event'] in ('reported', 'planned'):
                    if frame.get('failure') or any(
                            diagnostic.get('severity') == 'error'
                            for diagnostic in frame.get('diagnostics') or []):
                        raise RuntimeError(str(frame))
                if frame['event'] == 'planned' and frame['cell'] == cell:
                    planned = frame
                if frame['event'] == 'ready':
                    ready.add(frame['node'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/wes')
    parser.add_argument('--extractor', type=Path,
                        default=ROOT / 'tools/describe/wes-extract')
    parser.add_argument('--check', action='store_true',
                        help='Verify live setup and stop without opening a model client')
    args = parser.parse_args()
    binary, extractor = args.binary.resolve(), args.extractor.resolve()
    if not (ROOT / 'gui/dist/index.html').is_file():
        parser.error('Build the GUI first: npm run build')
    server = make_server(0)
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='wes-prom-workspace-') as directory:
            root = Path(directory).resolve()
            prepare(root, binary, extractor, server.server_port)
            process = subprocess.Popen(
                [str(binary), '--home', str(root / 'home'), '--serve', '0',
                 '--site', str(ROOT / 'gui/dist')], cwd=root,
                text=True, stdout=subprocess.PIPE)
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(process.stdout, selectors.EVENT_READ)
                    if not selector.select(30):
                        raise RuntimeError('Engine startup timed out')
                address = process.stdout.readline().strip()
                if not address.startswith('Listening at http://'):
                    raise RuntimeError(f'Engine did not start: {address}')
                seed(address.removeprefix('Listening at '), root)
                assert len(server.requests) == 1, server.requests
                if args.check:
                    print('PASS running GUI engine receives four finite steps and one HTTP query')
                    return
                print(address, flush=True)
                print('Synthetic API and values; real Wes commands and results.\n'
                      'Open the URL. Ctrl-C stops both servers and removes the workspace.',
                      flush=True)
                process.wait()
            except KeyboardInterrupt:
                pass
            finally:
                if process.poll() is None:
                    process.send_signal(signal.SIGINT)
                    try:
                        process.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)
                process.stdout.close()
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


if __name__ == '__main__':
    main()
