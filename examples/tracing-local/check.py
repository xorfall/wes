#!/usr/bin/env python3
"""Run the public local trace workflow with a synthetic file and generated storage key."""
import argparse
import json
import os
from pathlib import Path
import struct
import subprocess
import tempfile

here = Path(__file__).resolve().parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", type=Path, default=here.parents[1] / "target/debug/wes")
binary = parser.parse_args().binary.resolve()
with tempfile.TemporaryDirectory(prefix="wes-tracing-local-") as temporary:
    root = Path(temporary).resolve()
    key = root / "key"
    key.write_bytes(os.urandom(32))
    key.chmod(0o600)
    packet = bytes.fromhex("0200000000020200000000010800450000280007000040060000c000020ac6336414d4311f9000000064000000005002100000000000")
    original = struct.pack("<IHHIIII", 0xa1b2c3d4, 2, 4, 0, 0, 65535, 1) + struct.pack("<IIII", 1, 0, len(packet), len(packet)) + packet
    (root / "synthetic.pcap").write_bytes(original)
    source = (here / "workflow.wes").read_text().replace('"synthetic.pcap"', json.dumps(str(root / "synthetic.pcap"))).replace('"explicit.pcap"', json.dumps(str(root / "explicit.pcap")))
    script = root / "workflow.wes"
    script.write_text(source)
    common = [str(binary), "--home", str(root / "home"), "--storage-key-file", str(key), "--no-auto-keep", "--sequential"]
    run = subprocess.run([*common, "--env-file", str(here / "environment.yaml"), "--env", "local-traces", "--file", str(script)], cwd=root, text=True, capture_output=True, timeout=60)
    assert run.returncode == 0, run.stdout + run.stderr
    assert (root / "explicit.pcap").read_bytes() == original
    (root / "synthetic.pcap").unlink()
    reopened = subprocess.run([*common, "--command", ':read bytes:$saved dependency:"input" from:"0" limit:24 > reopened'], cwd=root, text=True, capture_output=True, timeout=30)
    assert reopened.returncode == 0, reopened.stdout + reopened.stderr
    for path in (root / "home").rglob("*"):
        if path.is_file():
            assert packet not in path.read_bytes(), path
print("PASS local trace: import, owned analysis, explicit export and retained dependency after reopen")
