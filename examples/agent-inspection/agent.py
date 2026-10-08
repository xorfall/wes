#!/usr/bin/env python3
"""Synthetic MCP participant; no model or network provider account."""
import json, os, subprocess, sys, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
root = Path(sys.argv[1])
process = subprocess.Popen([str(Path(os.environ['WES_ASSISTANT_DIRECTORY']) / 'wes-mcp')],
                           stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
sequence = 0
response_bytes = {}
def rpc(method, params=None):
    global sequence
    sequence += 1
    process.stdin.write(json.dumps({'jsonrpc':'2.0','id':sequence,'method':method,'params':params or {}})+'\n')
    process.stdin.flush()
    response = json.loads(process.stdout.readline())
    assert response['id'] == sequence and 'error' not in response, response
    return response['result']
def tool(name, arguments=None, error=False):
    if name in ('execute','cancel','tab_open'):
        arguments={'workspace':'default', **(arguments or {})}
    response = rpc('tools/call', {'name':name,'arguments':arguments or {}})
    assert bool(response.get('isError')) == error, response
    text = response['content'][0]['text']
    response_bytes[name] = len(text.encode())
    return text if error else json.loads(text)
def signal(name, value=None):
    temp = root / (name+'.tmp'); temp.write_text(json.dumps(value or {})); temp.replace(root/name)
def wait(name):
    end = time.monotonic()+20
    while time.monotonic()<end:
        if (root/name).exists(): return
        time.sleep(.02)
    raise AssertionError(name)
def execute(identity, source):
    response = tool('execute', {'source':source,'context':context['context'],'request_id':identity,'wait_ms':1000})
    while not response.get('settled'):
        response = tool('execution_read', {'request_id':identity,'wait_ms':1000})
    return response
try:
    rpc('initialize', {'protocolVersion':'2025-11-25','clientInfo':{},'capabilities':{}})
    process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n');process.stdin.flush()
    context = tool('workspace_context')
    human = tool('cell_read', {'cell':'human-failed','source':True})
    assert human['source']['available'] is False, human
    assert human['diagnostics'][0]['message'] and 'subject' not in human['diagnostics'][0], human
    signal('identity.json', {'actor':context['actor']})
    wait('granted.json')
    human = tool('cell_read', {'cell':'human-failed','source':True})
    assert human['source']['text'] == ':inspect $human_missing', human
    assert human['diagnostics'][0]['subject'] == 'human_missing' or human['diagnostics'][0]['subject'] == '$human_missing', human
    signal('read.json');wait('revoked.json')
    assert tool('cell_read', {'cell':'human-failed','source':True})['source']['available'] is False
    dataset_node = None
    for name in ['prepare.wes','analyze.wes']:
        response = execute(name, (HERE/name).read_text())
        if name == 'prepare.wes': dataset_node = response['nodes'][0]['node']
        assert not response['diagnostics'], response
        assert all(n['state']=='ready' for n in response['nodes']), response
        assert all('result' not in n for n in response['nodes']), response
    own = tool('cell_read', {'cell':response['cell'],'source':True})
    assert own['source']['text'] == (HERE/'analyze.wes').read_text(), own
    before = tool('workspace_snapshot')['total']
    assert tool('value_read', {'name':'summary'}) == {'total':5000,'errors':500,'sumIds':1247500}
    summary_bytes = response_bytes['value_read']
    metadata = tool('value_read', {'name':'dataset','select':'/rows','shape_only':True})
    assert metadata['length']==5000 and 'value' not in metadata, metadata
    page = tool('value_read', {'name':'dataset','select':'/rows','offset':4998,'limit':2})
    assert [r['id'] for r in page['value']]==[4998,4999], page
    assert page['page']=={'offset':4998,'count':2,'total':5000,'next_offset':None}, page
    page_bytes = response_bytes['value_read']
    assert tool('value_read', {'name':'dataset','select':'/rows/20/status'})['value']==503
    typed = tool('value_read', {'name':'dataset','select':'/rows/20/id','typed':True})
    assert typed['value']['data']['kind']=='int', typed
    for reference in ['dataset', '$dataset', dataset_node, '$'+dataset_node]:
        assert tool('value_read', {'name':reference,'select':'/rows/20/status'})['value']==503
    assert 'No data result' in tool('value_read', {'name':dataset_node+'::error'}, error=True)
    for args in [{'select':'/absent'}, {'select':'/rows','offset':5001}]:
        error = tool('value_read', {'name':'dataset',**args}, error=True)
        assert error.startswith('invalid value selection: ') and 'stored data' not in error, error
    for name, args, expected in [
        ('value_read', {'name':'dataset','limit':0}, 'arguments.limit must be at least 1'),
        ('execute', {'source':'PRIVATE_SENTINEL','context':'TOKEN_SENTINEL'}, 'request_id'),
        ('pane_command', {}, 'command'),
        ('value_read', {'name':'dataset','typed':'PRIVATE_SENTINEL'}, 'arguments.typed must be boolean'),
        ('value_read', {'name':'dataset','PRIVATE_SENTINEL':True}, 'unexpected field'),
        ('not_a_tool', {}, 'Unknown tool name'),
    ]:
        message = tool(name,args,error=True)
        assert expected in message and 'PRIVATE_SENTINEL' not in message and 'TOKEN_SENTINEL' not in message, message
    matching = tool('help', {'command':'calc','tail':['iter.matches']})
    assert matching['path']=='calc iter.matches', matching
    assert matching['invocation']['returns']=='Iter<Text>', matching
    assert 'not capture groups' in matching['invocation']['summary'], matching
    assert [p['type'] for p in matching['invocation']['parameters']]==['Text','Text'], matching
    captures = tool('help', {'command':'calc','tail':['iter.captures']})
    assert captures['invocation']['returns']=='Iter<Record {match:Text, groups:List<Option<Text>>}>', captures
    assert 'full match is excluded' in captures['invocation']['behavior'], captures
    assert tool('workspace_snapshot')['total']==before

    # Read a real forensic stream through the advertised MCP boundary, including
    # a parent with zero ordinary outputs. No read creates or replays a producer.
    source = (
        ':package load source:"types: {SkippedStep: {base: Record, fields: {state: Int, outputs: \'List<Int>\'}}}"\n'
        ':def skippedCount(state:Int, context:Int, item:Unknown) -> SkippedStep as :calc pure { return {state:state+1,outputs:[]}; }\n'
        ':calc pure { const half=' + json.dumps('z' * 32768) + '; return half + half + \"z\"; } > skippedRaw\n'
        ':scan source:$skippedRaw transition:skippedCount initial:0 context:0 '
        'profile:LinesUtf8 sink:dataset malformed:forensic excerpt:3 > skippedAnalysis'
    )
    forensic = execute('forensic-analysis', source)
    assert not any(item['severity']=='error' for item in forensic['diagnostics']) and all(node['state']=='ready' for node in forensic['nodes']), forensic
    before_reads = tool('workspace_snapshot')['total']
    summary = tool('dataset_inspect', {'name':'skippedAnalysis','select':'/outputs'})
    assert summary['stream']=='outputs' and summary['reference']['records']=='0', summary
    assert summary['coverage']['records']=='1' and summary['coverage']['inputBytes']=='65537', summary
    coverage = tool('dataset_page', {'name':'skippedAnalysis','select':'/outputs','stream':'coverage','from':'0','limit':1})
    assert coverage['stream']=='coverage' and coverage['reference']==summary['reference'], coverage
    assert coverage['page']['next']=='1' and coverage['page']['extentExhausted'], coverage
    row = coverage['page']['rows'][0]
    assert row['ordinal']=='0' and row['value']['data']['recordOrdinal']=='0', row
    assert row['value']['data']['reason']=='raw_limit' and row['value']['data']['excerpt']=='enp6', row
    assert row['value']['data']['unterminated'] is True
    outputs = tool('dataset_page', {'name':'skippedAnalysis','select':'/outputs'})
    assert outputs['stream']=='outputs' and outputs['page']['rows']==[], outputs
    assert 'outputs' in tool('dataset_page', {'name':'skippedAnalysis','stream':'unknown'}, error=True)
    assert tool('workspace_snapshot')['total']==before_reads
    clean = execute('clean-analysis', ':scan source:\"clean\" transition:skippedCount initial:0 context:0 profile:LinesUtf8 sink:dataset > cleanAnalysis')
    assert all(node['state']=='ready' for node in clean['nodes']), clean
    missing_stream = tool('dataset_page', {'name':'cleanAnalysis','select':'/outputs','stream':'coverage'}, error=True)
    assert 'no rejected-frame coverage' in missing_stream and 'public typed' not in missing_stream, missing_stream

    assert tool('validate', {'source':':calc { const count=8; return count; }'})['valid']
    shadowing = execute('lexical-shadowing', (HERE.parent/'lexical-shadowing/program.wes').read_text())
    assert not shadowing['diagnostics'], shadowing
    assert tool('value_read', {'name':'report'})=={'duration':5,'count':3,'helper':7,'values':[6,7],'method':2,'localCall':9,'workspace':20}
    unnamed = execute('unnamed-data', ':calc { return {status: 201}; }')
    anonymous_node = unnamed['nodes'][0]['node']
    assert tool('value_read', {'name':anonymous_node})=={'status':201}
    assert tool('value_read', {'name':'$'+anonymous_node,'select':'/status'})['value']==201
    assert 'No data result' in tool('value_read', {'name':'$id999999999'},error=True)

    full = tool('value_read', {'name':'dataset'})
    assert len(full['rows'])==5000
    full_bytes=response_bytes['value_read']
    assert page_bytes < 1024 and summary_bytes < 128 and full_bytes > 400000, (page_bytes,summary_bytes,full_bytes)
    missing = execute('missing-provider', ':inspect absent_provider operation')
    message=missing['diagnostics'][0]['message']
    assert 'absent_provider' in missing['diagnostics'][0]['subject'] and 'catalogue' in message, missing
    failed = execute('missing-name', ':inspect $missing_name')
    assert failed['diagnostics'][0]['code']=='PLN001' and 'not defined' in failed['diagnostics'][0]['message'], failed
    assert 'missing_name' in failed['diagnostics'][0]['subject'], failed
    assert tool('cell_read', {'cell':failed['cell'],'source':True})['source']['available']
    signal('metrics.json', {'full_bytes':full_bytes,'page_bytes':page_bytes,'summary_bytes':summary_bytes})
    print('INSPECTION_OK', flush=True)
finally:
    process.stdin.close()
    try: process.wait(timeout=5)
    except subprocess.TimeoutExpired: process.terminate();process.wait(timeout=5)
