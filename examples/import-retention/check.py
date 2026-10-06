#!/usr/bin/env python3
"""Exercise the checked-in import files using synthetic HOME; never call the declared service."""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from fixture_environment import isolated

parser = argparse.ArgumentParser()
parser.add_argument('--binary', type=Path, required=True)
args = parser.parse_args()
binary = args.binary.resolve()
with tempfile.TemporaryDirectory(prefix='wes-import-retention-') as temporary:
    root = Path(temporary).resolve()
    user = root / 'user'
    user.mkdir()
    inputs = root / 'inputs'
    shutil.copytree(Path(__file__).parent, inputs)
    environment = isolated(user)
    home = user / '.wes'
    def run(*arguments, diagnostics=False):
        result = subprocess.run([str(binary), '--home', str(home), *arguments], cwd=root,
                                env=environment, text=True, capture_output=True, timeout=30)
        assert result.returncode == 0, result.stdout + result.stderr
        if diagnostics:
            assert 'ENV000' in result.stderr, result.stderr
            return result.stderr.split('ENV000', 1)[1]
        return result.stdout
    def data(*arguments):
        output = run(*arguments)
        return json.loads(output.split(': ', 1)[1])
    run('--file', str(inputs / 'load.wes'))
    run('--env-file', str(inputs / 'environments.yaml'), '--command', ':inspect env:archive_demo')
    inspected = data('--command', ':inspect env:archive_demo')
    environment_revision = inspected['revision']
    providers = data('--command', ':list providers')
    types = data('--command', ':list types')
    rendered = data('--command', ':calc pure { return "Hello " + $greeting; }')
    run('--command', ':workspace save "imported"')
    for name, relative in [('catalog.yaml', 'imports/packages/{}.yaml'),
                           ('environments.yaml', 'imports/packages/{}.yaml'),
                           ('service.json', 'api-library/objects/{}.json')]:
        source = (inputs / name).read_bytes()
        revision = hashlib.sha256(source).hexdigest()
        assert (home / relative.format(revision)).read_bytes() == source
    receipts = [json.loads(p.read_text()) for p in (home / 'imports/records').glob('*.json')]
    assert {r['kind'] for r in receipts} == {'spec', 'types', 'environments'}
    for receipt in receipts:
        assert (home / receipt['object']).is_file()
        assert os.path.samefile(Path(receipt['origin']).parent, inputs)
    shutil.rmtree(inputs)
    moved = root / 'relocated'
    home.rename(moved)
    home = moved
    before = {p.relative_to(home): p.stat().st_mtime_ns for p in (home / 'imports').rglob('*') if p.is_file()}
    run('--workspace', 'imported', '--command', ':calc pure { return "Hello " + $greeting; }')
    after = {p.relative_to(home): p.stat().st_mtime_ns for p in (home / 'imports').rglob('*') if p.is_file()}
    assert before == after, 'replay wrote a new capture'
    # These copies are an archive, not the engine's recovery source.
    shutil.rmtree(home / 'imports')
    shutil.rmtree(home / 'api-library' / 'objects')
    assert data('--workspace', 'imported', '--command', ':calc pure { return "Hello " + $greeting; }') == rendered
    assert data('--workspace', 'imported', '--command', ':list types') == types
    assert data('--workspace', 'imported', '--command', ':list providers') == providers
    assert data('--workspace', 'imported', '--command', ':inspect env:archive_demo') == inspected
    scoped = data('--workspace', 'imported', '--env', 'archive_demo', '--env-revision', environment_revision, '--command', ':list providers')
    assert any(p['provider'] == 'inventory' and p['environment'] == 'archive_demo' for p in scoped)
    assert not (home / 'imports').exists(), 'replay recreated source archives'
    assert not list((home / 'api-library' / 'objects').glob('*.json')), 'replay recreated descriptors'
print('PASS: environments, types and APIs restore without original files or archive copies')
