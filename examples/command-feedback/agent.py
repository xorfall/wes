#!/usr/bin/env python3
"""Synthetic agent: real MCP calls, no account, source checkout reads or provider service."""
import json, os, subprocess, sys, time
from pathlib import Path
root = Path(sys.argv[1])
baseline = '--baseline' in sys.argv
process = subprocess.Popen([str(Path(os.environ['WES_ASSISTANT_DIRECTORY'])/'wes-mcp')], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
sequence = 0
calls = 0
records = []
def rpc(method, params=None):
    global sequence
    sequence += 1
    process.stdin.write(json.dumps({'jsonrpc':'2.0','id':sequence,'method':method,'params':params or {}})+'\n');process.stdin.flush()
    response = json.loads(process.stdout.readline())
    assert response['id'] == sequence and 'error' not in response, response
    return response['result']
def tool(name, arguments=None):
    if name in ('execute','cancel','tab_open'):
        arguments={'workspace':'default', **(arguments or {})}
    global calls
    calls += 1
    result = rpc('tools/call', {'name':name,'arguments':arguments or {}})
    text = result['content'][0]['text']
    return (text if result.get('isError') else json.loads(text)), len(text.encode())
def signal(name, value):
    tmp=root/(name+'.tmp');tmp.write_text(json.dumps(value));tmp.replace(root/name)
def wait(name):
    end=time.monotonic()+20
    while time.monotonic()<end:
        if (root/name).exists():return
        time.sleep(.02)
    raise AssertionError(name)
try:
    rpc('initialize', {'protocolVersion':'2025-11-25','clientInfo':{},'capabilities':{}})
    process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n');process.stdin.flush()
    context,_ = tool('workspace_context')
    hidden,_ = tool('cell_read', {'cell':'human-failed','source':True})
    if not baseline:
        assert hidden['diagnostics'][0]['message']=='Unexpected command path word. The environment name must be a quoted text operand.', hidden
        assert 'subject' not in hidden['diagnostics'][0], hidden
    signal('identity.json', {'actor':context['actor']});wait('granted.json')
    visible,_=tool('cell_read', {'cell':'human-failed','source':True})
    if not baseline:
        assert visible['diagnostics'][0]['subject']=='private_environment', visible
        assert visible['diagnostics'][0]['message']==hidden['diagnostics'][0]['message'], visible
    signal('read.json', {});wait('revoked.json')
    revoked,_=tool('cell_read', {'cell':'human-failed','source':True})
    assert 'subject' not in revoked['diagnostics'][0], revoked
    cases = [
        (':env', 'Missing environment subcommand.', None),
        (':env wat', 'Unknown environment subcommand.', 'wat'),
        (':env use claude_dev', 'Unexpected command path word.', 'claude_dev'),
        (':env use', 'Expected one quoted environment name.', None),
        (':env use "missing"', 'Unknown environment name.', 'missing'),
        (':env rename "a" to:b to:c', 'Duplicate environment argument key.', 'to'),
        (':env use typo:a', 'Unexpected environment argument key.', 'typo'),
        (':env use $secret', 'Expected an environment name, received a reference.', 'secret'),
        (':env clear > answer', 'This environment control has no output to bind.', '> answer'),
        (':env apply', 'Missing environment plan reference.', None),
        (':env apply "literal"', 'Environment plan operand must be a reference.', 'literal'),
        (':env apply $unknown', 'No live environment plan', 'unknown'),
        (':env plan > proposed', 'Missing plan input.', None),
        (':env plan file:a source:b > proposed', 'Conflicting plan inputs.', 'b'),
        (':env plan file:a base:b > proposed', 'base: requires source:', 'b'),
        (':enw', 'Unknown meta command.', 'enw'),
        (':list types mystery:foo', 'Unexpected argument key', 'mystery'),
        (':list types mystery:foo mystery:bar', 'Duplicate argument key.', 'mystery'),
        (':inspect $missing', 'Referenced name is not defined', 'missing'),
        (':inspect env:default', None, None),
        (':env plan source:"version: 1\\nenvironments: {}\\n" > proposed', None, None),
        (':env discard $proposed', None, None),
        (':env discard $proposed', 'No live environment plan', 'proposed'),
        (':env plan source:"version: 1\\nenvironments: {}\\n" > proposed', None, None),
        (':env discard $proposed', None, None),
        (':calc { return 42; } > answer', None, None),
    ]
    # Stress bounds without increasing exported source/context or requiring help/validate calls.
    cases += [(':env use '+word, 'Unexpected command path word.', word[:160]) for word in ['a'*20000, 'ç'*12000]]
    for index,(source,reason,subject) in enumerate(cases):
        before=calls
        reply,size=tool('execute', {'source':source,'context':context['context'],'request_id':f'feedback-{index}','wait_ms':1000})
        if isinstance(reply,dict):
            context['context']=reply.get('context',context['context'])
            execution=reply.get('execution',reply)
            while not execution.get('settled'):
                reply,size=tool('execution_read', {'request_id':f'feedback-{index}','wait_ms':1000})
                execution=reply.get('execution',reply)
            if not baseline:
                errors=[d for d in execution['diagnostics'] if d['severity']=='error']
                if reason:
                    assert errors and errors[0]['message'].startswith(reason), (source[:100],execution)
                    if subject:assert subject in errors[0]['subject'], (source[:100],execution)
                else:
                    assert not errors, (source,execution)
                    assert all(n['state']=='ready' for n in execution['nodes']), execution
                assert size<5000, size
            records.append({'source':source,'reply':execution,'bytes':size,'calls':calls-before})
        else:
            assert baseline, (source,reply)
            records.append({'source':source,'error':reply,'bytes':size,'calls':calls-before})
    refused,_=tool('execute', {'source':':env disable "default"','context':context['context'],'request_id':'refused','wait_ms':1000})
    assert isinstance(refused,str) and 'require user authority' in refused, refused
    if not baseline:
        help_reply,_=tool('help', {'command':'env','tail':['use']})
        assert help_reply['invocation']['operands']['min']==1, help_reply
        assert help_reply['invocation']['parameters']==[{'name':'revision','type':'Text','required':False}], help_reply
    signal('records.json', records)
    print('FEEDBACK_OK',flush=True)
finally:
    process.stdin.close()
    try:process.wait(timeout=5)
    except subprocess.TimeoutExpired:process.terminate();process.wait(timeout=5)
