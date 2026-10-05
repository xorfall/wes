#!/usr/bin/env python3
"""Model-free MCP participant, launched by check-shared.py in a real isolated PTY."""
import json, os, subprocess, sys, time

# Publish synchronization evidence only after the complete JSON is visible.
def publish(path, text):
    staged = path.with_suffix('.pending')
    staged.write_text(text)
    staged.replace(path)

from pathlib import Path

role, directory = sys.argv[1:]
root = Path(directory)
process = subprocess.Popen([str(Path(os.environ['WES_ASSISTANT_DIRECTORY']) / 'wes-mcp')], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
sequence = 0

def rpc(method, params=None):
    global sequence
    sequence += 1
    process.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': sequence, 'method': method, 'params': params or {}}) + '\n')
    process.stdin.flush()
    result = json.loads(process.stdout.readline())
    assert result['id'] == sequence and 'error' not in result, result
    return result['result']

def tool(name, arguments=None, error=False):
    if name in ('execute', 'cancel', 'tab_open'):
        arguments = {'workspace':'default', **(arguments or {})}
    if name in ("execute", "execution_read"): arguments = {"results":"full", **(arguments or {})}
    result = rpc('tools/call', {'name': name, 'arguments': arguments or {}})
    assert bool(result.get('isError')) == error, result
    text = result['content'][0]['text']
    return text if error else json.loads(text)

def done(request, workspace=None):
    args = {'request_id': request, **({'workspace': workspace} if workspace else {})}
    end = time.monotonic() + 15
    while time.monotonic() < end:
        result = tool('execution_read', args)
        if result.get('nodes') and all(n.get('state') == 'ready' for n in result['nodes']): return result
        time.sleep(.02)
    raise AssertionError(result)

def wait_file(name):
    end = time.monotonic() + 40
    while time.monotonic() < end:
        path = root / name
        if path.exists(): return json.loads(path.read_text())
        time.sleep(.03)
    raise AssertionError('Timed out waiting for ' + name)

try:
    rpc('initialize', {'protocolVersion': '2025-11-25', 'clientInfo': {}, 'capabilities': {}})
    process.stdin.write(json.dumps({'jsonrpc': '2.0', 'method': 'notifications/initialized'}) + '\n'); process.stdin.flush()
    original = tool('workspace_context')
    assert original['workspace'] == 'default'
    snapshot = tool('workspace_snapshot', {'limit': 1})
    assert len(snapshot['cells']) == 1 and 'source' not in snapshot['cells'][0]
    human = tool('cell_read', {'cell': 'human-seed', 'source': True})
    assert human['nodes'][0]['result']['value'] == {'seed': 41}, human
    assert human['source']['available'] is False
    assert tool('help', {'command': 'refresh'})['assistant_allowed'] is True
    assert tool('help', {'command': 'list'})['assistant_allowed'] is True
    tool('workspace_snapshot', {'limit': 101}, error=True)
    tool('workspace_context', {'workspace': 'shared-analysis'}, error=True)
    if role == 'a':
        joined = tool('workspace_open', {'name': 'shared-analysis', 'create': True})
        assert tool('workspace_open', {'name': 'shared-analysis'}) == joined
        assert joined['context'].startswith('ctx:') and len(joined['context']) == 68
        context = joined['context']
        tool('execute', {'workspace': 'shared-analysis', 'context': original['context'], 'request_id': 'wrong-context', 'source': ':calc { return 0; }'}, error=True)
        args = {'workspace': 'shared-analysis', 'context': context, 'request_id': 'same-id', 'source': ':calc { return 7; } > shared_result'}
        tool('execute', args)
        result = done('same-id', 'shared-analysis')
        assert result['nodes'][0]['result']['value'] == 7
        assert tool('execute', args)['duplicate']
        tool('execution_read', {'request_id': 'same-id'}, error=True)
        assert 'shared_result' not in tool('values_list')
        layout = tool('layout_read'); assert layout['ok'] and layout['layout']['panes'][0]['id'] == 'p1'
        opened = tool('tab_open', {'workspace': 'shared-analysis', 'pane': 'p1'})
        assert opened['ok'], opened
        tool('draft_read', {'workspace': 'shared-analysis'}, error=True)
        publish(root / 'a-ready.json', json.dumps({'cell': result['cell'], 'generation': joined['generation']}))
        wait_file('b-joined.json')
        print('SHARED_A_READY', flush=True)
        # Parent closes this terminal; B must keep working in the shared session.
        time.sleep(40)
    else:
        created = wait_file('a-ready.json')
        joined = tool('workspace_open', {'name': 'shared-analysis'})
        assert joined['generation'] == created['generation']
        tool('cancel', {'workspace': 'shared-analysis', 'request_id': 'same-id'}, error=True)
        tool('execution_read', {'workspace': 'shared-analysis', 'request_id': 'same-id'}, error=True)
        assert tool('cell_read', {'workspace': 'shared-analysis', 'cell': created['cell']})['nodes'][0]['result']['value'] == 7
        context = joined['context']
        tool('execute', {'workspace': 'shared-analysis', 'context': context, 'request_id': 'same-id', 'source': ':calc { return 9; } > own_result'})
        own = done('same-id', 'shared-analysis')
        assert own['nodes'][0]['result']['value'] == 9
        publish(root / 'b-joined.json', '{}')
        wait_file('a-closed.json')
        assert tool('value_read', {'workspace': 'shared-analysis', 'name': 'shared_result'}) == 7
        assert tool('workspace_context')['workspace'] == 'default'
        # A reload invalidates the captured target generation, without ending the origin.
        publish(root / 'b-ready-reload.json', '{}')
        wait_file('reloaded.json')
        tool('workspace_context', {'workspace': 'shared-analysis'}, error=True)
        assert tool('workspace_context')['workspace'] == 'default'
        assert tool('workspace_open', {'name': 'shared-analysis'})['generation'] != joined['generation']
        # Explicit rejoin restores request correlation, not old mutation/source authority.
        restored = tool('execution_read', {'workspace': 'shared-analysis', 'request_id': 'same-id'})
        assert restored['cell'] == own['cell']
        print('SHARED_B_OK', flush=True)
finally:
    if process.poll() is None:
        process.stdin.close()
        try: process.wait(timeout=5)
        except subprocess.TimeoutExpired: process.terminate(); process.wait(timeout=5)
