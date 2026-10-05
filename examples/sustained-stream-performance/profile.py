#!/usr/bin/env python3
"""Sample only a benchmark child PID created here; never attach by application name."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, default=ROOT/'target/release/examples/measure-sustained-streams')
parser.add_argument('--output-dir', type=Path, required=True)
args = parser.parse_args()
binary = args.binary.resolve(strict=True)
output = args.output_dir.resolve()
output.mkdir(parents=True, exist_ok=False)
with (output/'result.json').open('w') as stdout, (output/'stderr.txt').open('w') as stderr:
    process = subprocess.Popen([str(binary), 'engine', 'chain', '20', '256', '1000', 'complete'],
                               cwd=ROOT, stdout=stdout, stderr=stderr)
    try:
        time.sleep(3)
        assert process.poll() is None
        sample = subprocess.run(['/usr/bin/sample', str(process.pid), '10', '1', '-mayDie',
                                 '-file', str(output/'stacks.txt')], capture_output=True,
                                text=True, timeout=30)
        code = process.wait(timeout=60)
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=10)
metadata = {'git_head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
            'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
            'pid': process.pid, 'profile_exit_code': sample.returncode, 'trial_exit_code': code,
            'sample_duration_seconds': 10, 'sample_interval_ms': 1,
            'note': 'Wall-clock stack sampling includes blocked threads; excluded from throughput medians.',
            'profiler_stdout': sample.stdout, 'profiler_stderr': sample.stderr}
(output/'environment.json').write_text(json.dumps(metadata, indent=2)+'\n')
assert sample.returncode == 0 and code == 0
assert json.loads((output/'result.json').read_text())['success']
raw = (output/'stacks.txt').read_bytes()
(output/'stacks.txt.gz').write_bytes(gzip.compress(raw, mtime=0))
assert gzip.decompress((output/'stacks.txt.gz').read_bytes()) == raw
print(f'Owned benchmark stack sample complete: {output}')
