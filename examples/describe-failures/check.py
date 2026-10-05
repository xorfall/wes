#!/usr/bin/env python3
"""Actual Go -> Rust -> durable private-report acceptance; no network or model."""
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
with tempfile.TemporaryDirectory(prefix='wes-failure-report-') as directory:
    base = Path(directory).resolve()
    home = base / 'home'
    for name in ['openapi.json', 'describe.wes']:
        shutil.copyfile(HERE / name, base / name)
    def run(*args, body=None):
        return subprocess.run([str(ROOT / 'target/debug/wes'), '--home', str(home), *args],
                              cwd=base, input=body, capture_output=True, text=True, timeout=30)
    def action(value):
        result = run('--api-request', '-', body=json.dumps(value))
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout)
    initialized=run('--command', ':help')
    assert initialized.returncode == 0, initialized.stderr
    action({'action':'configure','expectedRevision':action({'action':'status'})['revision'],'settings':{'localDirectory':str(home/'api-library'),
            'extractor':str(ROOT/'tools/describe/wes-extract')}})
    run('--file', 'describe.wes')
    reports = list((home/'diagnostics/describe').glob('*.json'))
    assert len(reports) == 1, 'failure report must survive process exit'
    details = action({'action':'describeFailure','id':reports[0].stem})
    assert any('oneOf' in issue['message'] for issue in details['report']['issues']), details
    assert not (base/'must-not-exist.json').exists()
    assert action({'action':'list'})['packages'] == []
    assert reports[0].stat().st_mode & 0o777 == 0o600
print('PASS real extractor failure, private durable details and no spec publication')
