#!/usr/bin/env python3
"""Synthetic dumpcap-compatible pipe fixture; never opens an interface or socket."""
import argparse
import json
import os
from pathlib import Path
import signal
import struct
import subprocess
import sys
import time

PAYLOAD = b"synthetic-local-capture-sentinel\x00\xff"


def main():
    p = argparse.ArgumentParser()
    for flag in ("-i", "-f", "-F", "-s", "-c", "-a", "-w"):
        p.add_argument(flag, required=True)
    p.add_argument("-q", action="store_true")
    p.add_argument("-p", action="store_true")
    args = p.parse_args()
    assert args.F == "pcap" and args.w == "-" and args.q
    assert os.environ.get("LC_ALL") == "C" and "SYNTHETIC_SECRET" not in os.environ
    with open("attempts", "a") as file:
        file.write("1\n")
    Path("backend.pid").write_text(str(os.getpid()))
    Path("backend.arguments.json").write_text(json.dumps(sys.argv[1:]))
    stopped = False
    def stop(_sig, _frame):
        nonlocal stopped
        if args.i != "synthetic-ignore-stop":
            stopped = True
            Path("backend.stopped").write_text("joined request")
    signal.signal(signal.SIGINT, stop)
    print("Capturing on 'synthetic'", file=sys.stderr, flush=True)
    if args.i == "synthetic-slow-start":
        Path("backend.banner").write_text("startup banner only")
        while not Path("allow-ready").exists() and not stopped:
            time.sleep(0.005)
    if args.i == "synthetic-diagnostics":
        print("x" * 4097, file=sys.stderr, flush=True)
        while not stopped:
            time.sleep(0.01)
        return 0
    if args.i == "synthetic-invalid-header":
        sys.stdout.buffer.write(b"invalid-container-header" + b"!")
        sys.stdout.buffer.flush()
        return 0
    endian, power = (">", 9) if args.i == "synthetic-big-endian" else ("<", 6)
    magic = b"\xa1\xb2\x3c\x4d" if power == 9 else b"\xd4\xc3\xb2\xa1"
    sys.stdout.buffer.write(magic + struct.pack(endian + "HHiIII", 2, 4, 0, 0, int(args.s), 1))
    sys.stdout.buffer.flush()
    if args.i != "synthetic-no-ack":
        print("File: -", file=sys.stderr, flush=True)
    if args.i == "synthetic-child-pipe":
        child = subprocess.Popen([sys.executable, "-c", "import signal,time; signal.signal(signal.SIGINT, signal.SIG_IGN); time.sleep(30)"])
        Path("backend.child.pid").write_text(str(child.pid))
    if args.i in {"synthetic-idle", "synthetic-no-ack", "synthetic-ignore-stop"}:
        while not stopped:
            time.sleep(0.005)
        return 0
    if args.i == "synthetic-held-packet":
        while not Path("allow-packet").exists() and not stopped:
            time.sleep(0.005)
    count = 513 if args.i in {"synthetic-index-window", "synthetic-large"} else 1
    payload = (PAYLOAD + bytes([147]) * (65535 - len(PAYLOAD)) if args.i == "synthetic-large" else PAYLOAD)[:int(args.s)]
    for n in range(count):
        original = len(payload) + (7 if args.i == "synthetic-truncated" else 0)
        header = struct.pack(endian + "IIII", 17 + n, 11, len(payload), original)
        if args.i == "synthetic-malformed-packet":
            header = struct.pack(endian + "IIII", 17, 10 ** power, len(payload), len(payload))
        sys.stdout.buffer.write(header)
        if args.i == "synthetic-partial-packet":
            sys.stdout.buffer.write(payload[:-1])
        else:
            sys.stdout.buffer.write(payload)
        sys.stdout.buffer.flush()
    if args.i in {"synthetic-hold", "synthetic-slow-start", "synthetic-child-pipe", "synthetic-held-packet"}:
        while not stopped:
            time.sleep(0.005)
    return 3 if args.i == "synthetic-failed" else 0


if __name__ == "__main__":
    sys.exit(main())
