#!/usr/bin/env python3
"""Synthetic agent: its own child shell discovers and uses inherited workspace tools."""
import json
import os
import subprocess


def tool(command):
    result = subprocess.run(['/bin/sh', '-c', command], capture_output=True, text=True, timeout=20)
    assert result.returncode == 0, (command, result.returncode, result.stderr)
    assert not result.stderr, result.stderr
    return json.loads(result.stdout)


assert os.isatty(0) and os.isatty(1), 'agent did not inherit a real terminal'
assert 'lab-sensors' in tool('wesx provider --list')['providers']
assert 'lab-sensors' in tool('wes-provider --list')['providers']  # compatibility
assert 'readings' in tool('wesx value list')
assert tool('lab-sensors --help')['provider'] == 'lab-sensors'
assert tool('wesx value get readings')['body']['temperature'] == 21.25
assert tool('wesx value get readings --wes-typed')['format'] == 'wes.value'
assert tool('lab-sensors status')['body']['readings']['temperature'] == 21.25
assert subprocess.check_output(['/bin/sh', '-c', 'printf NATIVE'], text=True) == 'NATIVE'
assert tool('wesx provider printf readings')['body']['temperature'] == 21.25
assert 'WES_FIXTURE_SECRET' not in os.environ, 'ambient engine secret leaked into shell'
print('SYNTHETIC_AGENT_OK', flush=True)
