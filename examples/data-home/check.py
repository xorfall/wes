#!/usr/bin/env python3
"""Check the actual example in a synthetic user home; no desktop or Keychain access."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument('--binary', type=Path, required=True)
args = parser.parse_args()
binary = args.binary.resolve()
example = Path(__file__).with_name('calculation.wes')
with tempfile.TemporaryDirectory(prefix='wes-data-home-example-') as temporary:
    root = Path(temporary)
    user = root / 'user'
    user.mkdir()
    environment = {**os.environ, 'HOME': str(user)}
    def run(*arguments):
        result = subprocess.run([str(binary), *arguments], cwd=root, env=environment,
                                text=True, capture_output=True, timeout=30)
        assert result.returncode == 0, result.stderr
        return result.stdout
    run('--file', str(example))
    home = user / '.wes'
    before = json.loads((home / 'identity.json').read_text())
    run('--command', ':workspace save "example"')
    assert 'example' in run('--command', ':list workspaces')
    moved = root / 'moved-data'
    home.rename(moved)
    run('--home', str(moved), '--workspace', 'example', '--command', ':inspect $answer')
    assert json.loads((moved / 'identity.json').read_text()) == before
    settings = json.loads((moved / 'api-library-settings.json').read_text())
    assert settings['version'] == 1 and settings['settings']['localDirectory'] == 'api-library'
    assert (moved / 'api-library/.wes-api-library-v1').is_file()
    assert list((moved / 'values/archive').glob('*.json'))
print('PASS: default home, saved workspace, retained values and identity survive relocation')
