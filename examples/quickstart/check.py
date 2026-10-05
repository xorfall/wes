#!/usr/bin/env python3
"""Run the documented program in an isolated home and verify its actual output."""
import argparse, json, subprocess, tempfile
from pathlib import Path
here = Path(__file__).resolve().parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, default=here.parents[1] / 'target/debug/wes')
binary = parser.parse_args().binary.resolve()
with tempfile.TemporaryDirectory(prefix='wes-quickstart-') as home:
    run = subprocess.run([str(binary), '--home', home, '--file', str(here / 'main.wes')],
                         text=True, capture_output=True, timeout=30)
    assert run.returncode == 0, run.stdout + run.stderr
    values = [json.loads(line.partition(': ')[2]) for line in run.stdout.splitlines()
              if line.startswith('id') and ': ' in line]
    assert values == [42, 43], run.stdout
print('PASS quickstart: named result and dependent calculation')
