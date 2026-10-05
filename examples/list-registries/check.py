#!/usr/bin/env python3
"""Check the actual view package and discovery script in an isolated CLI data home."""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, required=True)
args = parser.parse_args()
binary = args.binary.resolve()
with tempfile.TemporaryDirectory(prefix='wes-list-registries-') as folder:
    root = Path(folder)
    def run(*arguments):
        result = subprocess.run([str(binary), '--json', '--home', str(root / 'home'), *arguments],
                                cwd=root, capture_output=True, text=True, timeout=30)
        assert result.returncode == 0, result.stderr
        return [json.loads(line.split(': ', 1)[1]) for line in result.stdout.splitlines()]
    run('--file', str(HERE / 'definitions.wes'))
    def verify(values):
        definitions, definition, help_value = values
        assert 'Summary' in definitions, definitions
        assert definition['purity'] == 'pure' and definition['conversionEligible'] is True, definition
        metadata = {r['name']: r for r in help_value['invocation']['listing']['registries']}
        assert 'views' in metadata and 'renderers' not in metadata
        assert metadata['capabilities']['filters'] == ['provider']
        assert set(metadata) == set(help_value['invocation']['takes'])
    verify(run('--file', str(HERE / 'discover.wes')))
    run('--command', ':workspace save "saved"')
    verify(run('--workspace', 'saved', '--file', str(HERE / 'discover.wes')))
    assert run('--workspace', 'empty', '--command', ':list templates') == [[]]
print('PASS actual registry example: typed definition purity, help scope/filter parity, saved restore, workspace isolation')
