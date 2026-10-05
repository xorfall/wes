#!/usr/bin/env python3
"""Actual MCP wire measurement: native reduction, raw shell, optimized shell; synthetic data only."""
import base64, json, os, shlex, subprocess, sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
root = Path(sys.argv[1])
env = dict(os.environ, WES_MCP_METRICS_DIR=str(root/'mcp-reports'))
process = subprocess.Popen([str(Path(os.environ['WES_ASSISTANT_DIRECTORY'])/'wes-mcp')],
                           env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
sequence = 0
phase = 'setup'
cost = {}
context = {}

def rpc(method, params=None):
    global sequence
    sequence += 1
    request = json.dumps(dict(jsonrpc='2.0', id=sequence, method=method, params=params or {}))+'\n'
    process.stdin.write(request); process.stdin.flush()
    wire = process.stdout.readline()
    response = json.loads(wire)
    assert response['id'] == sequence and 'error' not in response, response
    bucket = cost.setdefault(phase, dict(calls=0, request_bytes=0, response_bytes=0, result_bytes=0, tools=[]))
    bucket['calls'] += 1
    bucket['request_bytes'] += len(request.encode())
    bucket['response_bytes'] += len(wire.encode())
    if method == 'tools/call':
        bucket['tools'].append(params['name'])
        bucket['result_bytes'] += len(response['result']['content'][0]['text'].encode())
    return response['result']

def tool(name, arguments=None):
    if name in ('execute','cancel','tab_open'):
        arguments={'workspace':'default', **(arguments or {})}
    result = rpc('tools/call', dict(name=name, arguments=arguments or {}))
    assert not result.get('isError'), result
    return json.loads(result['content'][0]['text'])

def execute(identity, source, ready=True):
    global context
    result = tool('execute', dict(source=source, context=context['context'], request_id=identity, wait_ms=1000))
    if 'context' in result:
        context['context'] = result['context']
    while not result.get('settled'):
        result = tool('execution_read', dict(request_id=identity, wait_ms=1000))
    assert not any(d.get('severity') == 'error' for d in result.get('diagnostics', [])), result
    if ready:
        assert all(n['state'] == 'ready' for n in result['nodes']), result
    return result

try:
    rpc('initialize', dict(protocolVersion='2025-11-25', clientInfo={}, capabilities={}))
    process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n'); process.stdin.flush()
    context = tool('workspace_context')
    assert context['providers'] == ['docker', 'http', 'sh'], context
    help = tool('help', dict(command='import', tail=['docker']))
    assert help['path'] == 'import docker' and 'socket:<Text>' in help['invocation']['usage'], help
    # A new alias is agent-owned; replacing the host's default binding needs real authority.
    execute('connect', ':import docker socket:'+json.dumps(str(root/'docker.sock'))+' as:diagnostics')
    phase = 'native'
    execute('native-investigation', (HERE/'investigate.wes').read_text())
    summary = tool('value_read', dict(name='diagnosis'))
    assert summary['exit_code'] == 137 and summary['oom_killed'] and summary['restarts'] == 7, summary
    assert summary['allocation_failures'] == 12 and summary['lines'] == 200, summary
    assert not summary['truncated'] and len(summary['examples']) == 2, summary
    assert len(json.dumps(summary).encode()) < 4096
    # Both baselines run the checked-in script against exactly the same fake daemon.
    for mode in ['raw', 'summary']:
        phase = 'shell_'+mode
        command = 'python3 '+shlex.quote(str(HERE/'shell.py'))+' '+mode+' '+shlex.quote(str(root/'docker.sock'))
        execute(phase, 'sh run cmd:'+json.dumps(command)+' > '+phase)
        result = tool('value_read', dict(name=phase))
        assert result['exitCode'] == 0, result
        stdout = json.loads(base64.b64decode(result['stdout']))
        if mode == 'summary':
            assert stdout == summary, (stdout, summary)
        else:
            assert len(stdout['logs']) == 200
    assert cost['native']['response_bytes'] < cost['shell_raw']['response_bytes'], cost
    assert 'must-not-export' not in json.dumps(summary), summary
    (root/'measurement.json').write_text(json.dumps(dict(schema='wes.docker-investigation.v1',
        units='UTF-8 bytes; not model tokens', costs=cost, summary=summary), indent=2))
finally:
    process.stdin.close()
    try: process.wait(timeout=10)
    except subprocess.TimeoutExpired: process.terminate(); process.wait(timeout=5)
print('DOCKER_INVESTIGATION_OK', flush=True)
