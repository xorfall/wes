#!/usr/bin/env python3
"""Isolated real-PTY output/latency acceptance. No model, services or user values."""
import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import selectors
import signal
import statistics
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[2]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/wes')
    parser.add_argument('--legacy', action='store_true', help='Measure the previous 100 ms UI cadence without long-poll support')
    args = parser.parse_args()
    payload = b''.join(f'\x1b[32m{i:05d} '.encode() + b'x' * 60 + b'\x1b[0m\r\n' for i in range(6000))
    with tempfile.TemporaryDirectory(prefix='wes-terminal-performance-') as directory:
        root = Path(directory)
        (root / 'payload.bin').write_bytes(payload)
        (root / 'terminal-fixture.py').write_text('''import os, tty
from pathlib import Path
tty.setraw(0)
payload = Path("payload.bin").read_bytes()
os.write(1, b"READY\\n")
while True:
    command = os.read(0, 1)
    if command == b"Q": break
    data = payload if command == b"B" else command
    while data:
        data = data[os.write(1, data):]
''')
        with (root / 'stderr').open('w+') as errors:
            process = subprocess.Popen([str(args.binary.resolve()), '--home', str(root / 'home'), '--serve', '0', '--no-auto-keep'], cwd=root, stdout=subprocess.PIPE, stderr=errors, text=True)
            try:
                selector = selectors.DefaultSelector(); selector.register(process.stdout, selectors.EVENT_READ)
                assert selector.select(20), 'server startup timed out'
                url = process.stdout.readline().strip().removeprefix('Listening at ')
                events = urllib.request.urlopen(url + '/events', timeout=10)
                generation = None
                while not generation:
                    line = events.readline().decode()
                    if line.startswith('data:'):
                        value = json.loads(line[5:])
                        if value['event'] == 'session': generation = value['generation']
                def call(action, **values):
                    body = dict(client='performance-fixture', action=action, **values)
                    request = urllib.request.Request(url + '/terminals', json.dumps(body).encode(), {'Content-Type': 'application/json', 'X-Wes-Session': generation})
                    with urllib.request.urlopen(request, timeout=10) as response: return json.load(response)
                identity = call('start')['id']; cursor = 0
                def poll(wait=1000, at=None):
                    extra = {} if args.legacy else {'wait_ms': wait}
                    return call('poll', id=identity, cursor=cursor if at is None else at, **extra)
                def consume(frame):
                    nonlocal cursor
                    assert frame['start'] == cursor, ('lost output', cursor, frame['start'])
                    data = base64.b64decode(frame['data']); assert len(data) <= 65536
                    assert frame['next'] == cursor + len(data)
                    cursor = frame['next']; return data
                def read_until(size=None, marker=None):
                    result = b''; end = time.monotonic() + 15
                    while time.monotonic() < end:
                        result += consume(poll())
                        if (size is not None and len(result) >= size) or (marker is not None and marker in result): return result
                        if args.legacy: time.sleep(.1)
                    raise AssertionError('terminal data timed out')
                call('write', id=identity, text='python3 terminal-fixture.py\r')
                read_until(marker=b'READY\n')
                assert not consume(poll(0)), 'unexpected fixture output'
                if not args.legacy:
                    began = time.monotonic(); frame = poll(120)
                    assert not consume(frame) and time.monotonic() - began >= .08, 'idle reader spun'
                    with ThreadPoolExecutor() as pool:
                        pending = pool.submit(poll)
                        time.sleep(.06); assert not pending.done(), 'empty long poll did not wait'
                        try:
                            poll()
                            raise AssertionError('parallel waiting reader admitted')
                        except urllib.error.HTTPError as error: assert error.code == 400
                        call('write', id=identity, text='!')
                        assert consume(pending.result(timeout=2)) == b'!', 'wake lost'
                    try:
                        poll(1001)
                        raise AssertionError('unbounded wait admitted')
                    except urllib.error.HTTPError as error: assert error.code == 400
                samples = []
                for _ in range(20):
                    began = time.monotonic(); call('write', id=identity, text='e')
                    assert read_until(size=1) == b'e'
                    samples.append((time.monotonic() - began) * 1000)
                before = cursor
                began = time.monotonic(); call('write', id=identity, text='B')
                assert read_until(size=len(payload)) == payload, 'colored burst reordered or corrupted'
                burst_ms = (time.monotonic() - began) * 1000
                replay = poll(0, before)
                assert base64.b64decode(replay['data']) == payload[:65536], 'read replay was not exact'
                if not args.legacy:
                    with ThreadPoolExecutor() as pool:
                        pending = pool.submit(poll); time.sleep(.05)
                        assert not pending.done()
                        call('close', id=identity)
                        assert pending.result(timeout=2)['closed'], 'close did not wake reader'
                else: call('close', id=identity)
                print(json.dumps({'mode': 'legacy-100ms' if args.legacy else 'notification', 'echo_median_ms': round(statistics.median(samples), 2), 'echo_max_ms': round(max(samples), 2), 'burst_bytes': len(payload), 'burst_ms': round(burst_ms, 2)}))
                print('PASS: real PTY echo, ordered bounded colored burst, cursor replay, idle/wake/close and waiter limits')
                events.close()
            finally:
                process.send_signal(signal.SIGINT)
                try: process.wait(timeout=15)
                except subprocess.TimeoutExpired: process.kill(); process.wait()
                if process.returncode != 0:
                    errors.seek(0); raise AssertionError(errors.read())

if __name__ == '__main__': main()
