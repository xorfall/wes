#!/usr/bin/env python3
"""Run the local credential lesson. Never supply a real service key to this fixture."""
import argparse
from getpass import getpass
import json
from pathlib import Path
import subprocess
from lesson_support import DEMO_TOKEN

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--home', type=Path, required=True)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/wes')
    parser.add_argument('--env-file', type=Path, default=HERE / 'auth.environments.yaml')
    parser.add_argument('--demo-token', action='store_true', help='Use the public synthetic token')
    args = parser.parse_args()
    token = DEMO_TOKEN if args.demo_token else getpass('Synthetic demo token (never a real key): ')
    if not token:
        parser.error('a non-empty synthetic token is required')
    result = subprocess.run([
        str(args.binary.resolve()), '--home', str(args.home.resolve()),
        '--env-file', str(args.env_file.resolve()), '--env', 'lesson',
        '--grant-provider', 'secureApi', '--credentials-stdin', '--file', str(HERE / 'auth.wes'),
    ], input=json.dumps({'tutorial/inventory/token': token}), text=True, timeout=30, check=False)
    return result.returncode


if __name__ == '__main__':
    raise SystemExit(main())
