#!/usr/bin/env python3
"""Launch the complete live monitor in a fresh temporary workspace; Ctrl-C closes both services."""
import argparse
from contextlib import ExitStack
from pathlib import Path
import tempfile
import time
from fixture import DemoServer, sample
from support import Client, ROOT, copy_project


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/wes')
    parser.add_argument('--interval', type=float, default=.5)
    args = parser.parse_args()
    if args.interval <= 0:
        parser.error('--interval must be positive')
    if not (ROOT / 'gui/dist/index.html').is_file():
        parser.error('Build the GUI first: cd gui && npm run build')
    server = DemoServer()
    try:
        with ExitStack() as stack:
            folder = stack.enter_context(tempfile.TemporaryDirectory(prefix='wes-monitor-demo-'))
            root = Path(folder)
            copy_project(root, server.server_port)
            client = Client(args.binary.resolve(), root, site=True)
            stack.callback(client.close)
            client.prepare()
            print(f'Live monitor: {client.url}\nOpen this URL. Inspect health_display, request_table, latency_timeline and latency_histogram.\n20 normal -> 20 degraded -> 20 recovered requests, repeating.\nIsolated temporary workspace: {root}\nCtrl-C stops the demo and removes its temporary workspace.', flush=True)
            seq = 1
            while client.process.poll() is None:
                if client.problem:
                    raise RuntimeError(client.problem)
                server.publish([sample(seq)])
                seq += 1
                time.sleep(args.interval)
    except KeyboardInterrupt:
        pass
    finally:
        server.close()


if __name__ == '__main__':
    main()
