#!/usr/bin/env python3
"""Real app + PTY + plain agent names + stdio MCP. Synthetic agents and local API only."""
import argparse, base64, importlib.util, json, os, re, selectors, shlex, subprocess, sys, tempfile, threading, time, urllib.request, uuid
from pathlib import Path
HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[1]
spec=importlib.util.spec_from_file_location('sensor_fixture', HERE.parent/'api-workflow/fixture.py')
fixture=importlib.util.module_from_spec(spec); spec.loader.exec_module(fixture)

def main():
    if sys.version_info < (3, 11):
        raise SystemExit('This fixture requires Python 3.11 or newer; select that interpreter before running it.')
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,default=ROOT/'target/debug/wes')
    binary=parser.parse_args().binary.resolve()
    api=fixture.make_server(0); thread=threading.Thread(target=api.serve_forever,daemon=True);thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='wes-assistant-check-') as directory:
            root=Path(directory).resolve(); fake=root/'bin';fake.mkdir()
            for name in ['claude','opencode','codex']:
                path=fake/name
                path.write_text('#!/bin/sh\nexec '+shlex.quote(sys.executable)+' '+shlex.quote(str(HERE/'agent.py'))+' '+name+' "$@"\n');path.chmod(0o700)
            env=os.environ.copy();env['PATH']=str(fake)+os.pathsep+env.get('PATH','')
            env.pop('WES_MCP_METRICS_DIR', None)
            codex_directory = root / '.codex'; codex_directory.mkdir()
            codex_config = codex_directory / 'config.toml'
            codex_original = 'model = "fixture-model"\napproval_policy = "on-request"\nsandbox_mode = "workspace-write"\n'
            codex_config.write_text(codex_original)
            for key in ['ANTHROPIC_API_KEY','OPENAI_API_KEY']:env.pop(key,None)
            recipe=(HERE/'environments.yaml').read_text().replace('../api-workflow/sensor.json',str(HERE.parent/'api-workflow/sensor.json')).replace('http://127.0.0.1:8771',f'http://127.0.0.1:{api.server_port}')
            (root/'environments.yaml').write_text(recipe)
            with (root/'server.log').open('w+') as errors:
                app=subprocess.Popen([str(binary),'--home',str(root/'home'),'--serve','0'],cwd=root,env=env,stdout=subprocess.PIPE,stderr=errors,text=True)
                events=None
                try:
                    with selectors.DefaultSelector() as selector:
                        selector.register(app.stdout,selectors.EVENT_READ);assert selector.select(60),'server startup timeout'
                    line=app.stdout.readline().strip();assert line.startswith('Listening at http://'),line
                    url=line.removeprefix('Listening at ')
                    events=urllib.request.urlopen(url+'/events',timeout=15)
                    binding_errors = set()
                    def event_until(predicate):
                        while True:
                            line=events.readline().decode();assert line,'event stream ended'
                            if line.startswith('data:'):
                                event=json.loads(line[5:])
                                if event['event']=='reported':
                                    diagnostics = event.get('diagnostics', [])
                                    if event.get('source') == 'missing_fixture_provider operation':
                                        assert [d['code'] for d in diagnostics] == ['RES004'], event
                                        binding_errors.add(event['cell'])
                                    else:
                                        assert not any(d.get('severity')=='error' for d in diagnostics), event
                                if predicate(event):return event
                    generation=event_until(lambda e:e['event']=='session')['generation'];client='assistant-fixture-ui';context=None
                    def post(path,data):
                        request=urllib.request.Request(url+path,json.dumps(data).encode(),{'Content-Type':'application/json','X-Wes-Session':generation})
                        with urllib.request.urlopen(request,timeout=25) as response:return json.load(response)
                    def source(text):return post('/submit',{'request':'submit','cell':str(uuid.uuid4()),'text':text,'client':client,'environments':context})
                    def terminal(action,**kwargs):return post('/terminals',{'action':action,'client':client,**kwargs})
                    source(':env plan file:environments.yaml > setup');source(':env apply $setup')
                    environments=event_until(lambda e:e['event']=='environments' and 'assistant_demo' in e['revisions'])
                    context={'selected':'assistant_demo','revisions':environments['revisions']};source(':env use "assistant_demo"')
                    types_file = root/'assistant.types.yaml'
                    types_file.write_text('types: {AssistantCustomer: {base: Record, fields: {name: Text}}}')
                    source(':package load path:'+json.dumps(str(types_file)))
                    source((HERE/'prepare.wes').read_text())
                    created=event_until(lambda e:e['event']=='created' and e.get('name')=='assistant_seed')
                    event_until(lambda e:e['event']=='ready' and e['node']==created['node'])
                    identity=terminal('start')['id'];cursor=0;output='';draft={'text':'user draft','revision':'initial'};replies={};pane_commands=[]
                    def write(text):terminal('write',id=identity,text=text+'\r')
                    def wait(marker):
                        nonlocal cursor,output,draft
                        deadline=time.monotonic()+60
                        while time.monotonic()<deadline:
                            frame=terminal('poll',id=identity,cursor=cursor,wait_ms=1000);cursor=frame['next'];output+=base64.b64decode(frame['data']).decode('utf-8','replace')
                            if frame.get('command'):
                                request=frame['command'];rid=request['id']
                                assert terminal('commandclaim',id=identity,request=rid)['claimed']
                                assert not terminal('commandclaim',id=identity,request=rid)['claimed']
                                pane_commands.append(request['text'])
                                assert request['text'] in ['/rsplit xterm','/split','/close'],request
                                terminal('commandreply',id=identity,request=rid,
                                    error='Four panes are already open.' if request['text']=='/split' else None)
                            if frame.get('editor'):
                                request=frame['editor'];rid=request['id']
                                if rid not in replies:
                                    if request['action']=='read':replies[rid]={'ok':True,**draft}
                                    elif request['revision']!=draft['revision']:replies[rid]={'ok':False,'error':'Draft changed'}
                                    else:draft={'text':request['text'],'revision':str(uuid.uuid4())};replies[rid]={'ok':True,**draft}
                                terminal('editorreply',id=identity,request=rid,result=replies[rid])
                            if re.search(re.escape(marker)+r'\r*\n',output):return
                            assert not frame['closed'],output[-5000:]
                            time.sleep(.03)
                        raise AssertionError(output[-9000:])
                    write('stty -echo');write("claude --fixture 'literal $(not-a-command)'");wait('ASSISTANT_CLAUDE_OK')
                    assert api.requests==['/history?sensor=LAB1'],api.requests
                    draft={'text':'user draft','revision':str(uuid.uuid4())}
                    settings=json.dumps({'model':'synthetic/model','permission':'ask','mcp':{'other':{'enabled':False}}})
                    write('OPENCODE_CONFIG_CONTENT='+shlex.quote(settings)+" opencode --fixture 'literal $(not-a-command)'");wait('ASSISTANT_OPENCODE_OK')
                    assert api.requests==['/history?sensor=LAB1']*2,api.requests
                    draft={'text':'user draft','revision':str(uuid.uuid4())}
                    metrics = root / 'metrics'
                    write('WES_MCP_METRICS_DIR='+shlex.quote(str(metrics))+" codex --model synthetic/model --ask-for-approval on-request --sandbox workspace-write --fixture 'literal $(not-a-command)'")
                    wait('ASSISTANT_CODEX_OK')
                    write('WES_FIXTURE_LAUNCH_ONLY=1 codex resume fixture-session --model synthetic/model');wait('ASSISTANT_CODEX_RESUME_OK')
                    write('WES_AGENT_BYPASS=1 codex resume fixture-session --model synthetic/model');wait('ASSISTANT_CODEX_BYPASS_OK')
                    assert api.requests==['/history?sensor=LAB1']*3,api.requests
                    assert pane_commands==['/rsplit xterm','/split','/close']*3,pane_commands
                    assert codex_config.read_text() == codex_original
                    reports = list(metrics.glob('mcp-*.json')); assert len(reports) == 1, reports
                    report = json.loads(reports[0].read_text())
                    assert report['state'] == 'closed', report
                    assert report['buckets']['tools/execute']['messages'] > 0
                    assert report['totals']['output_bytes'] > 0
                    chart=event_until(lambda e:e['event']=='created' and e.get('name')=='codex_chart')
                    event_until(lambda e:e['event']=='ready' and e['node']==chart['node'])
                    assert len(binding_errors) == 3, binding_errors
                    # Export restrictions continue to apply through the exact same terminal.
                    context={'selected':'assistant_private','revisions':environments['revisions']};source(':env use "assistant_private"')
                    write("WES_FIXTURE_PRIVATE=1 claude --fixture 'literal $(not-a-command)'");wait('ASSISTANT_PRIVATE_OK')
                    source('sensor history sensor:LAB1 > private_readings')
                    created=event_until(lambda e:e['event']=='created' and e.get('name')=='private_readings');event_until(lambda e:e['event']=='ready' and e['node']==created['node'])
                    write("wesx value get private_readings; printf 'PRIVATE_EXIT=%s\\n' \"$?\"");wait('PRIVATE_EXIT=1')
                    assert 'cannot be exported' in output
                    # Copy only this fixture's private attachment for a revoked-token probe.
                    write('cp "$WES_ASSISTANT_DIRECTORY/wes-mcp" '+shlex.quote(str(root/'old-mcp'))+"; printf 'COPIED\\n'");wait('COPIED')
                    terminal('close',id=identity)
                    probe=subprocess.run([str(root/'old-mcp')],input='\n'.join([
                        json.dumps({'jsonrpc':'2.0','id':1,'method':'initialize','params':{'protocolVersion':'2025-11-25','clientInfo':{},'capabilities':{}}}),
                        json.dumps({'jsonrpc':'2.0','method':'notifications/initialized'}),
                        json.dumps({'jsonrpc':'2.0','id':2,'method':'tools/call','params':{'name':'workspace_context','arguments':{}}}),
                        json.dumps({'jsonrpc':'2.0','id':3,'method':'tools/call','params':{'name':'pane_command','arguments':{'command':'/split'}}})])+'\n',capture_output=True,text=True,timeout=20)
                    assert all(json.loads(line)['result']['isError'] for line in probe.stdout.splitlines()[-2:]),probe.stdout
                    print('PASS plain Claude/OpenCode/Codex launch, config/argv/cwd, resume/bypass, metrics forwarding, stdio MCP, scoped discovery, visible execution, no duplicate calls, chart/trace, draft CAS, pane command/error/close acknowledgements, privacy and revocation')
                finally:
                    if events:events.close()
                    app.terminate()
                    try:app.wait(timeout=15)
                    except subprocess.TimeoutExpired:app.kill();app.wait()
    finally:
        api.shutdown();api.server_close();thread.join()
if __name__=='__main__':main()
