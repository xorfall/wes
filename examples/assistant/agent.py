#!/usr/bin/env python3
"""Synthetic MCP client; never starts a model or uses user credentials."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import tomllib

kind, arguments = sys.argv[1], sys.argv[2:]
codex_server = None
if kind == 'codex':
    if os.environ.get('WES_AGENT_BYPASS') == '1':
        assert arguments == ['resume', 'fixture-session', '--model', 'synthetic/model'], arguments
        print('ASSISTANT_CODEX_BYPASS_OK', flush=True)
        sys.exit(0)
    assert arguments[0:6:2] == ['-c'] * 3, arguments
    config = tomllib.loads('\n'.join(arguments[1:6:2]))
    assert set(config) == {'mcp_servers'}, config
    codex_server = config['mcp_servers']['wes_workspace']
    assert set(codex_server) == {'command', 'args', 'env_vars'}, codex_server
    assert codex_server['args'] == []
    assert codex_server['env_vars'] == ['WES_MCP_METRICS_DIR']
    assert codex_server['command'] == str(Path(os.environ['WES_ASSISTANT_DIRECTORY']) / 'wes-mcp')
    arguments = arguments[6:]
    if os.environ.get('WES_FIXTURE_LAUNCH_ONLY') == '1':
        assert arguments == ['resume', 'fixture-session', '--model', 'synthetic/model'], arguments
        print('ASSISTANT_CODEX_RESUME_OK', flush=True)
        sys.exit(0)
    assert arguments[:-2] == ['--model', 'synthetic/model', '--ask-for-approval', 'on-request', '--sandbox', 'workspace-write'], arguments
    assert Path.cwd() == Path(os.environ['WES_MCP_METRICS_DIR']).parent
assert arguments[-2:] == ['--fixture', 'literal $(not-a-command)'], arguments
if kind == 'claude':
    assert arguments[:1] == ['--mcp-config'], arguments
    config = json.loads(Path(arguments[1]).read_text())
    command = [config['mcpServers']['wes_workspace']['command']]
elif kind == 'opencode':
    config = json.loads(os.environ['OPENCODE_CONFIG_CONTENT'])
    assert config['model'] == 'synthetic/model' and config['permission'] == 'ask'
    assert config['mcp']['other']['enabled'] is False
    command = config['mcp']['wes_workspace']['command']
else:
    command = [codex_server['command']]
# MCP clients may filter child environments. The private wrapper must still attach.
env = {k: v for k, v in os.environ.items() if not k.startswith('WES_')}
if codex_server:
    env.update({key: os.environ[key] for key in codex_server['env_vars'] if key in os.environ})
process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, env=env)
sequence = 0

def send(method, params=None):
    global sequence
    sequence += 1
    process.stdin.write(json.dumps({'jsonrpc':'2.0','id':sequence,'method':method,'params':params or {}})+'\n')
    process.stdin.flush()
    return sequence

def receive():
    line = process.stdout.readline()
    assert line, 'MCP subprocess closed'
    reply = json.loads(line)
    assert 'error' not in reply, reply
    return reply

def rpc(method, params=None):
    identity = send(method, params)
    reply = receive()
    assert reply['id'] == identity, reply
    return reply['result']

def tool(name, args=None, error=False):
    if name in ('execute', 'cancel', 'tab_open'):
        args = {'workspace':'default', **(args or {})}
    if name in ("execute", "execution_read"): args = {"results":"full", **(args or {})}
    reply = rpc('tools/call', {'name':name, 'arguments':args or {}})
    assert reply.get('isError', False) == error, reply
    text = reply['content'][0]['text']
    return text if error else json.loads(text)

def completed(request_id):
    end = time.monotonic()+10
    while time.monotonic()<end:
        reply = tool('execution_read', {'request_id':request_id, 'trace':True})
        if reply.get('nodes') and all(n['state'] not in ['pending','running'] for n in reply['nodes']):
            return reply
        time.sleep(.02)
    raise AssertionError(reply)

try:
    result = rpc('initialize', {'protocolVersion':'2025-11-25','clientInfo':{'name':'synthetic','version':'1'},'capabilities':{}})
    assert 'workspace_context' in result['instructions'] and 'pane_command' in result['instructions']
    process.stdin.write(json.dumps({'jsonrpc':'2.0','method':'notifications/initialized'})+'\n'); process.stdin.flush()
    names = [t['name'] for t in rpc('tools/list')['tools']]
    assert 'draft_update' in names and 'execute' in names and 'pane_command' in names
    assert 'view_authoring' in names
    authoring = tool('view_authoring')
    assert authoring['sdk'] == 2 and 'theme' in authoring['topics']
    theme = tool('view_authoring', {'topic':'theme'})['content']
    assert theme['roles']['table-value']['style']['font-family'].startswith('var(')
    assert '#' not in json.dumps(theme)
    assert 'defineView' in tool('view_authoring', {'topic':'sdk'})['content']['declarations']
    assert 'screen-title' in tool('view_authoring', {'topic':'examples'})['content']['files']['View.tsx']
    assert rpc('ping') == {}
    live = tool('workspace_context')
    if os.environ.get('WES_FIXTURE_PRIVATE') == '1':
        tool('execute', {'context':live['context'], 'request_id':'choose-private', 'source':':env use "assistant_private"', 'wait_ms':1000})
        live = tool('workspace_context')
    context = live['context']
    assert context.startswith('ctx:') and len(context) == 68
    assert 'guide' not in live and 'guide' not in tool('help')
    assert tool('workspace_context')['context'] == context
    assert 'sensor' in tool('workspace_context')['providers']
    discovery_id = kind+('-private' if os.environ.get('WES_FIXTURE_PRIVATE') == '1' else '')
    discovery_name = discovery_id.replace('-', '_')
    # Registry metadata is workspace-wide, including when the active environment changes.
    tool('execute', {'request_id':discovery_id+'-types','context':context,'source':f':list types > available_types_{discovery_name}'})
    types = completed(discovery_id+'-types')['nodes'][0]['result']['value']
    assert any(t['name']=='AssistantCustomer' and t['scope']=='workspace' for t in types), types
    assert any(t['name']=='List' and t['kind']=='constructor' for t in types), types
    tool('execute', {'request_id':discovery_id+'-type','context':context,'source':f':inspect type:AssistantCustomer > customer_type_{discovery_name}'})
    description = completed(discovery_id+'-type')['nodes'][0]['result']['value']
    assert description['kind']=='record' and description['fields'][0]['name']=='name', description
    captured = Path(os.environ['WES_ASSISTANT_DIRECTORY'])/'fixture-context.json'
    if os.environ.get('WES_FIXTURE_PRIVATE') == '1':
        old_context = captured.read_text()
        tool('execute', {'request_id':'changed-context','context':old_context,'source':':calc { return 1; }'}, error=True)
        tool('execute', {'request_id':'private-call','context':context,'source':'@trace(http) sensor history sensor:LAB1 > private_mcp_readings'})
        private = completed('private-call')['nodes'][0]
        assert not private['result']['available'] and not private['trace']['available'], private
        assert 'private_mcp_readings' not in tool('values_list')
        tool('value_read', {'name':'private_mcp_readings'}, error=True)
        print('ASSISTANT_PRIVATE_OK', flush=True)
        sys.exit(0)
    captured.write_text(context)
    tool('execute', {'request_id':'invalid-wait', 'context':context, 'source':':calc { return 0; }', 'wait_ms':1001}, error=True)
    tool('execution_read', {'request_id':'invalid-wait'}, error=True)
    waited = {'request_id':kind+'-waited', 'context':context, 'source':f':calc {{ return 42; }} > waited_answer_{kind}', 'wait_ms':1000}
    small = tool('execute', waited)
    assert small['execution']['settled'] and small['execution']['nodes'][0]['result']['value'] == 42, small
    repeated = tool('execute', {**waited, 'wait_ms':0})
    assert repeated['duplicate'] and repeated['execution']['cell'] == small['execution']['cell'], repeated
    assert tool('execution_read', {'request_id':waited['request_id'], 'wait_ms':1000})['settled']
    missing_target=rpc('tools/call',{'name':'execute','arguments':{'source':':calc { return 0; }','context':context,'request_id':'missing-target'}})
    assert missing_target['isError'] and 'workspace' in missing_target['content'][0]['text']
    name=kind.replace('-', '_')
    source=f""":calc {{ const window = interval(instant('2032-01-01T00:00:00Z'), instant('2032-01-01T00:01:00Z')); return {{metric:{{view:'timeline',id:'rate',title:'Rate',range:window,coverage:window,omitted:0,sourceError:'',series:[],events:[]}},layout:{{view:'timeline-group',title:'Activity',range:window}}}}; }} > sequence_data_{name}
:view create Timeline input:$sequence_data_{name}.metric > sequence_child_{name}
:view create TimelineGroup input:$sequence_data_{name}.layout > sequence_group_{name}
:view connect $sequence_child_{name} to:$sequence_group_{name}
:inspect $sequence_group_{name}"""
    args={'request_id':kind+'-sequence','context':context,'source':source,'sequential':True,'wait_ms':1000}
    result=tool('execute',args)['execution']
    if not result['settled']:result=tool('execution_read',{'request_id':args['request_id'],'wait_ms':1000})
    assert result['status']=='completed' and len(result['steps'])==5,result
    inspected=result['steps'][-1]['nodes'][0]['result']['value']
    assert len(inspected['view']['members']['members'])==1,inspected
    assert tool('execute',args)['duplicate']
    tool('execute',{**args,'sequential':False},error=True)
    forbidden=tool('execute',{'request_id':kind+'-sequence-preflight','context':context,'source':f':calc {{ return 1; }} > unstarted_{name}\n:env enable "production"','sequential':True},error=True)
    assert 'unstarted_'+name not in tool('values_list')
    # Admission diagnostics with no executable nodes must settle too.
    rejected = tool('execute', {'request_id':kind+'-binding-error','context':context,'source':'missing_fixture_provider operation','wait_ms':1000})
    assert rejected['execution']['settled'] and rejected['execution']['diagnostics'], rejected
    # Real stdio calls may complete out of order. One ID still admits only one cell.
    duplicate_args = {'workspace':live['workspace'], 'request_id':kind+'-concurrent', 'context':context, 'source':':calc { return 17; }'}
    duplicate_ids = {send('tools/call', {'name':'execute','arguments':duplicate_args}) for _ in range(2)}
    duplicates = [receive(), receive()]
    assert {r['id'] for r in duplicates} == duplicate_ids
    bodies = [json.loads(r['result']['content'][0]['text']) for r in duplicates]
    assert sorted(r['duplicate'] for r in bodies) == [False, True], bodies
    assert len({r['execution']['cell'] for r in bodies}) == 1, bodies
    assert tool('execution_read', {'request_id':duplicate_args['request_id'], 'wait_ms':1000})['settled']
    # A synthetic finite child process supplies genuinely pending work without live services.
    workspace = 'wiring-' + kind
    joined = tool('workspace_open', {'name':workspace, 'create':True})
    slow = {'workspace':workspace, 'request_id':kind+'-slow', 'context':joined['context'],
            'source':'sh run cmd:"sleep 5; printf fixture"', 'wait_ms':10}
    pending = tool('execute', slow)
    assert not pending['execution']['settled'], pending
    wait_id = send('tools/call', {'name':'execution_read','arguments':{'workspace':workspace,'request_id':slow['request_id'],'wait_ms':1000}})
    ping_id = send('ping')
    ping = receive()
    assert ping['id'] == ping_id and ping['result'] == {}, ping
    cancel_id = send('tools/call', {'name':'cancel','arguments':{'workspace':workspace,'request_id':slow['request_id']}})
    responses = [receive(), receive()]
    assert {r['id'] for r in responses} == {wait_id,cancel_id}, responses
    for response in responses:
        assert not response['result']['isError'], response
    cancelled = tool('execution_read', {'workspace':workspace,'request_id':slow['request_id'],'wait_ms':1000})
    assert cancelled['settled'] and cancelled['nodes'][0]['state'] == 'cancelled', cancelled
    list_help = tool('help', {'command':'list'})
    registries = {r['name']:r for r in list_help['invocation']['listing']['registries']}
    assert registries['templates']['scope'] == 'workspace' and registries['workspaces']['scope'] == 'home'
    assert registries['capabilities']['filters'] == ['provider']
    assert tool('help', {'command':'list','tail':['templates']})['invocation']['parameters'] == []
    native_help = tool('execute', {'request_id':kind+'-list-help','context':context,'source':':help list','wait_ms':1000})
    assert native_help['execution']['nodes'][0]['result']['value']['invocation']['listing'] == list_help['invocation']['listing']
    views = tool('execute', {'request_id':kind+'-views','context':context,'source':':list templates','wait_ms':1000})
    assert views['execution']['settled'] and views['execution']['nodes'][0]['result']['value'] == [], views
    metadata = tool('help', {'provider':'sensor','tail':['history']})
    assert metadata['invocation']['parameters'][0]['name'] == 'sensor'
    assert tool('value_read', {'name':'assistant_seed'}) == 7
    assert tool('validate', {'source':':calc { return 1; } > answer'})['valid']
    assert tool('validate', {'source':':env clear'})['valid']
    assert not tool('validate', {'source':':env enable "production"'})['valid']
    tool('execute', {'request_id':'pipeline', 'context':context, 'source':':calc { return 1; } | :calc { return input; }'})
    tool('execute', {'request_id':'forbidden', 'context':context, 'source':':env enable "production"'}, error=True)
    tool('execute', {'request_id':'stale', 'context':'old-context', 'source':':calc { return 1; }'}, error=True)
    tool('execution_read', {'request_id':'not-owned'}, error=True)
    tool('cancel', {'request_id':'not-owned'}, error=True)
    request = {'request_id':kind+'-call', 'context':context, 'source':f'@trace(http) sensor history sensor:LAB1 > {kind}_readings'}
    first = tool('execute',request)
    second = tool('execute',request)
    assert not first['duplicate'] and second['duplicate'] and first['execution']['cell']==second['execution']['cell']
    result = completed(request['request_id'])
    assert result['nodes'][0]['result']['value']['body'][1]['reading']==19.5, result
    assert result['nodes'][0]['trace']['available'], result
    tool('execute',{**request,'source':':calc { return 99; }'},error=True)
    chart = {'request_id':kind+'-chart','context':context,'source':f':calc pure {{ return {{view: "line", x: "time", y: "reading", points: ${kind}_readings.body.map(row => {{ return {{x: text(row.time), y: decimal(row.reading)}}; }}), dropped: 0}}; }} > {kind}_chart'}
    tool('execute',chart)
    result = completed(chart['request_id'])
    assert len(result['nodes'][0]['result']['value']['points'])==3
    draft=tool('draft_read')
    assert draft['ok'] and draft['text']=='user draft'
    changed=tool('draft_update',{'revision':draft['revision'],'text':':calc { return 42; } > draft_answer'})
    assert changed['ok'], changed
    conflict=tool('draft_update',{'revision':draft['revision'],'text':'must not replace newer draft'})
    assert not conflict['ok'], conflict
    assert 'draft_answer' not in tool('values_list'), 'Writing a draft executed it'
    # Same authenticated pane transport as wesx, with UI errors exposed as MCP tool errors.
    tool('pane_command', {}, error=True)
    tool('pane_command', {'command':'/split','unexpected':True}, error=True)
    tool('pane_command', {'command':'sensor history sensor:LAB1'}, error=True)
    assert tool('pane_command', {'command':'/rsplit xterm'}) == {'applied':True}
    assert 'Four panes' in tool('pane_command', {'command':'/split'}, error=True)
    assert tool('pane_command', {'command':'/close'}) == {'applied':True}
    print('ASSISTANT_'+kind.upper()+'_OK',flush=True)
finally:
    process.stdin.close()
    process.wait(timeout=10)
