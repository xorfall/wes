#!/usr/bin/env python3
"""Build the example's portable echo program before loading default.wes or environments.yaml."""
from pathlib import Path
import subprocess

here = Path(__file__).resolve().parent
subprocess.run(['rustc', '--edition=2024', str(here.parents[1] / 'support' / 'echo.rs'),
                '-o', str(here / 'example-echo.bin')], check=True)
