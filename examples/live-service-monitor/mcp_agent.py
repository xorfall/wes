#!/usr/bin/env python3
"""Model-free participant used only by check-mcp.py inside an isolated terminal."""
import json, os, subprocess, sys, time
from pathlib import Path
role, folder = sys.argv[1:]
root = Path(folder)
process = subprocess.Popen([str(Path(os.environ['WES_ASSISTANT_DIRECTORY'])/'wes-mcp')], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
serial = 0
calls = 0
reply_bytes = 0

def rpc(method, params=None):
    global serial
    serial += 1
    process.stdin.write(json.dumps({'jsonrpc':'2.0','id':serial,'method':method,'params':params or {}})+'\n'); process.stdin.flush()
    result = json.loads(process.stdout.readline())
    assert result['id']==serial and 'error' not in result, result
    return result['result']

def tool(name, arguments=None, error=False):
    if name in ('execute','cancel','tab_open'):
        arguments={'workspace':'default', **(arguments or {})}
    global calls, reply_bytes
    calls += 1
    result = rpc('tools/call', {'name':name,'arguments':arguments or {}})
    assert bool(result.get('isError')) == error, result
    text=result['content'][0]['text']; reply_bytes += len(text.encode())
    return text if error else json.loads(text)

def signal(name, value=None):
    pending=root/(name+'.tmp');pending.write_text(json.dumps(value or {}));pending.replace(root/name)
def wait(name):
    end=time.monotonic()+35
    while time.monotonic()<end:
        if (root/name).exists(): return json.loads((root/name).read_text())
        time.sleep(.02)
    raise AssertionError('Timed out: '+name)

def execute(identity, source, **extra):
    global context
    result=tool('execute',{'request_id':identity,'context':context,'source':source,'wait_ms':1000,**extra})
    context=result['context']
    execution=result['execution']
    assert execution['settled'], result
    return result

def success(result):
    execution=result['execution']
    assert 'error' not in execution and execution.get('status') != 'admission_failed', result
    assert not any(d['severity']=='error' for d in execution.get('diagnostics',[])), result
    assert not any(n.get('error') for n in execution.get('nodes',[])), result

def value(name, predicate=lambda x:True):
    end=time.monotonic()+20
    while time.monotonic()<end:
        result=rpc('tools/call', {'name':'value_read','arguments':{'name':name}})
        if not result.get('isError'):
            result=json.loads(result['content'][0]['text'])
            if predicate(result): return result
        time.sleep(.04)
    raise AssertionError((name,result))

try:
    rpc('initialize', {'protocolVersion':'2025-11-25','clientInfo':{},'capabilities':{}})
    process.stdin.write(json.dumps({'jsonrpc':'2.0','method':'notifications/initialized'})+'\n');process.stdin.flush()
    initial=tool('workspace_context');context=initial['context']
    signal(role+'-identity.json', initial)
    if role=='a':
        setup_start=calls; size_start=reply_bytes; started=time.monotonic()
        plan=execute('plan', f':env plan file:"{root / "environments.yaml"}" > monitor_plan');success(plan)
        assert plan['plans'][0]['changes'][0]['added'], plan
        for identity, source in [('apply', ':env apply $monitor_plan'), ('select', ':env use "monitor-demo"')]:
            applied = execute(identity, source); success(applied)
            notices = [d for d in applied['execution']['diagnostics'] if d['code'] == 'ENV000']
            assert notices and all(d['severity'] == 'info' and d['message'] == 'Informational notice; this is not an error.' for d in notices), applied
        assert tool('help',{'provider':'telemetry','tail':['watch']})['invocation']['streaming']
        success(execute('types',f':package load path:"{root / "types.yaml"}"'))
        source=(root/'monitor.wes').read_text()
        original_context=context
        monitor=execute('monitor',source,reactive=True);success(monitor)
        assert len(monitor['execution']['nodes'])==9, monitor
        assert any(n['streaming'] for n in monitor['execution']['nodes']),monitor
        assert all('result' not in n for n in monitor['execution']['nodes']),monitor
        setup={'calls':calls-setup_start,'reply_bytes':reply_bytes-size_start,'elapsed_ms':round((time.monotonic()-started)*1000,2),'scope':'MCP setup only, synthetic; not renderer performance'}
        duplicate=tool('execute',{'request_id':'monitor','context':original_context,'source':source,'reactive':True,'wait_ms':1000})
        assert duplicate['duplicate'] and duplicate['execution']['cell']==monitor['execution']['cell']
        for last,state in [(20,'HEALTHY'),(40,'DEGRADED'),(60,'HEALTHY'),(620,'HEALTHY')]:
            signal('phase.json',{'last':last});wait(f'published-{last}.json')
            health=value('health',lambda d:d['last']==last)
            assert health['state']==state and health['count']==20,health
            table=value('request_table',lambda d:len(d['rows'])==20 and d['rows'][-1][0]==str(last))
            line=value('latency_timeline',lambda d:len(d['series'][0]['samples'])==20 and d['series'][0]['samples'][-1]['id']==str(last))
            histogram=value('latency_histogram',lambda d:d['total']==len(table['rows']))
            assert histogram['total']==20 and sum(b['count'] for b in histogram['bins'])==20
            assert [p['value'] for p in line['series'][0]['samples']]==[int(r[2]) for r in table['rows']]
            value('health_display',lambda d:d.startswith(state))
            metric=value('health_metric',lambda d:d['status']==state and d['value']==health['meanMs'])
            assert metric['view']=='metric' and metric['unit']=='ms',metric
        window=value('requests',lambda d:len(d)==500 and d[-1]['seq']==620)
        assert window[0]['seq']==121
        changed=(root/'environments.yaml').read_text().replace('http://127.0.0.1:', 'http://localhost:')
        (root/'environments.yaml').write_text(changed)
        success(execute('replacement-plan',f':env plan file:"{root / "environments.yaml"}" reconcile:file > replacement'))
        replacement=execute('replacement-apply',':env apply $replacement')
        assert replacement['execution']['error']['code']=='AUT001',replacement
        reason=replacement['execution']['error']['message']
        assert 'shared environment definitions' in reason and 'user controls' in reason,reason
        assert str(root) not in reason and '127.0.0.1' not in reason,reason
        signal('a-ready.json',{'actor':initial['actor'],'setup':setup})
        wait('peer.json')
        reason=tool('cancel',{'request_id':'monitor'},error=True)
        assert 'protected work downstream' in reason and 'host controls' in reason,reason
        assert 'peer_count' not in reason and 'user_seed' not in reason,reason
        refresh=execute('protected-refresh',':refresh $requests')
        assert refresh['execution']['error']['code']=='AUT001',refresh
        reason=refresh['execution']['error']['message']
        assert 'protected work downstream' in reason and 'host controls' in reason,reason
        assert 'peer_count' not in reason and 'user_seed' not in reason,reason
        inspected=tool('cell_read',{'cell':refresh['execution']['cell'],'source':True})
        assert inspected['status']=='admission_failed' and inspected['settled'],inspected
        assert inspected['error']==refresh['execution']['error'],inspected
        assert inspected['source']['text']==':refresh $requests',inspected
        reread=tool('execution_read',{'request_id':'protected-refresh'})
        assert reread['error']==inspected['error'],reread
        signal('needs-grant.json');wait('granted.json')
        # Exact peer scope permits stopping the shared dependency, not touching unrelated user work.
        protected=execute('protected',':node remove $user_seed scope:downstream')
        assert any(d['code']=='AUT001' for d in protected['execution']['diagnostics']),protected
        observed = ['requests', 'recent', 'health', 'health_display', 'health_metric', 'request_table', 'latency_timeline', 'durations', 'latency_histogram']
        before_cancel = {name: value(name) for name in observed}
        assert json.loads(subprocess.check_output(['wesx', 'value', 'get', 'health'], text=True)) == before_cancel['health']
        assert tool('cancel',{'request_id':'monitor'})['cancellation_requested']
        for name in observed:
            stopped = value(name, lambda d: isinstance(d, dict) and d.get('status') == 'stopped')
            assert stopped['value'] == before_cancel[name] and stopped['run'] and stopped['source'], stopped
        shell_last = json.loads(subprocess.check_output(['wesx', 'value', 'get', 'health'], text=True))
        assert shell_last['status'] == 'stopped' and shell_last['value'] == before_cancel['health'], shell_last
        selected = tool('value_read', {'name':'requests','offset':0,'limit':2})
        assert selected['status'] == 'stopped' and selected['value']['value'] == before_cancel['requests'][:2], selected
        assert 'health' in tool('values_list')
        # Reading is allowed; using a stopped result as current calculation input is not.
        blocked = execute('stopped-input', ':calc { return $health.state; } > after_stop')
        assert blocked['execution']['nodes'] and all(n['state'] == 'skipped' for n in blocked['execution']['nodes']), blocked
        signal('cancelled.json');wait('revoked.json')
        refused=execute('revoked',':policy $peer_count mode:reactive')
        assert any(d['code']=='AUT001' for d in refused['execution']['diagnostics']),refused
        success(execute('survivor','telemetry watch feed:demo > surviving_requests'))
        signal('survivor.json')
        print('COOPERATIVE_A_OK',flush=True)
    else:
        wait('a-ready.json')
        live=tool('workspace_context');context=live['context']
        assert live['environment']['selected']==initial['environment']['selected']=='default',live
        assert 'telemetry' not in live['providers']
        rejected=execute('overwrite',':calc { return 0; } > user_seed')
        assert any(d['code']=='AUT001' for d in rejected['execution']['diagnostics']),rejected
        reason=next(d['message'] for d in rejected['execution']['diagnostics'] if d['code']=='AUT001')
        assert 'result name' in reason and 'new name' in reason and 'host controls' in reason,reason
        report=execute('peer',':calc { return length($requests); } > peer_count');success(report)
        signal('peer.json',{'cell':report['execution']['cell']})
        wait('a-closed.json')
        assert len(value('surviving_requests',lambda d:len(d)==500))==500
        assert value('user_seed')==41
        print('COOPERATIVE_B_OK',flush=True)
finally:
    process.stdin.close()
    try:process.wait(timeout=5)
    except subprocess.TimeoutExpired:process.terminate();process.wait(timeout=5)
