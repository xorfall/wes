#!/usr/bin/env python3
"""Real PTY/MCP acceptance: 270 requests, process restart, same saved pane, no model."""
import argparse, base64, json, os, selectors, shlex, subprocess, sys, tempfile, time, urllib.request, uuid
from pathlib import Path
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]

def agent(root, phase):
    client = subprocess.Popen([str(Path(os.environ['WES_ASSISTANT_DIRECTORY'])/'wes-mcp')], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    seq = 0
    def rpc(method, params=None):
        nonlocal seq
        seq += 1
        client.stdin.write(json.dumps({'jsonrpc':'2.0','id':seq,'method':method,'params':params or {}})+'\n'); client.stdin.flush()
        reply=json.loads(client.stdout.readline()); assert reply.get('id')==seq and 'error' not in reply,reply
        return reply['result']
    def tool(name,args=None):
        if name in ('execute','cancel','tab_open'):
            args={'workspace':'default', **(args or {})}
        reply=rpc('tools/call',{'name':name,'arguments':args or {}}); assert not reply.get('isError'),reply
        return json.loads(reply['content'][0]['text'])
    try:
        rpc('initialize',{'protocolVersion':'2025-11-25','clientInfo':{},'capabilities':{}})
        client.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n'); client.stdin.flush()
        ctx=tool('workspace_context'); source=(HERE/'request.wes').read_text()
        if phase=='first':
            first=None
            for i in range(270):
                response=tool('execute',{'request_id':f'r{i}','source':source,'context':ctx['context'],'wait_ms':1000})
                assert response['duplicate'] is False and response['execution']['settled'],response
                if first is None:first=response
            (root/'previous.json').write_text(json.dumps({'context':ctx,'first':first}))
        else:
            previous=json.loads((root/'previous.json').read_text()); old=previous['context']
            assert ctx['request_scope']==old['request_scope'] and ctx['actor']!=old['actor']
            cell=previous['first']['execution']['cell']
            assert tool('execution_read',{'request_id':'r0'})['cell']==cell
            duplicate=tool('execute',{'request_id':'r0','source':source,'context':old['context']})
            assert duplicate['duplicate'] and duplicate['execution']['cell']==cell,duplicate
            assert tool('workspace_snapshot')['total']==270
            assert not tool('cell_read',{'cell':cell,'source':True})['source']['available']
        print('REQUESTS_OK',flush=True)
    finally:
        client.stdin.close();client.wait(timeout=10)

def run(binary):
    with tempfile.TemporaryDirectory(prefix='wes-requests-') as directory:
        root=Path(directory); pane=str(uuid.uuid4())
        for phase in ['first','resumed']:
            with (root/(phase+'.log')).open('w+') as errors:
                app=subprocess.Popen([str(binary),'--home',str(root/'home'),'--serve','0','--no-auto-keep'],cwd=root,stdout=subprocess.PIPE,stderr=errors,text=True)
                events=None
                try:
                    with selectors.DefaultSelector() as selector:
                        selector.register(app.stdout,selectors.EVENT_READ); assert selector.select(30),'startup timeout'
                    url=app.stdout.readline().strip().removeprefix('Listening at ')
                    events=urllib.request.urlopen(url+'/events',timeout=20)
                    while True:
                        line=events.readline().decode();assert line,'closed event stream'
                        if line.startswith('data:'):
                            event=json.loads(line[5:])
                            if event['event']=='session':generation=event['generation'];break
                    def terminal(action,**args):
                        body={'client':'synthetic-user','action':action,**args}
                        request=urllib.request.Request(url+'/terminals',json.dumps(body).encode(),{'Content-Type':'application/json','X-Wes-Session':generation})
                        with urllib.request.urlopen(request,timeout=20) as response:return json.load(response)
                    identity=terminal('start',history=pane)['id']
                    command='python3 '+shlex.quote(str(HERE/'check.py'))+' --agent '+shlex.quote(str(root))+' '+phase+'\r'
                    terminal('write',id=identity,text=command)
                    cursor=0;output='';end=time.monotonic()+120
                    while time.monotonic()<end:
                        frame=terminal('poll',id=identity,cursor=cursor,wait_ms=100)
                        cursor=frame['next'];output+=base64.b64decode(frame['data']).decode('utf-8','replace')
                        if 'REQUESTS_OK\r\n' in output:break
                        assert 'Traceback (most recent call last)' not in output,output
                    assert 'REQUESTS_OK\r\n' in output,output
                    terminal('close',id=identity)
                finally:
                    if events:events.close()
                    app.terminate()
                    try:app.wait(timeout=15)
                    except subprocess.TimeoutExpired:app.kill();app.wait()
        print('PASS: 270 actual MCP requests; reopened pane finds old cell without rerun or restored source grants')

if __name__=='__main__':
    if len(sys.argv)>1 and sys.argv[1]=='--agent':agent(Path(sys.argv[2]),sys.argv[3])
    else:
        parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--binary',type=Path,default=ROOT/'target/debug/wes')
        run(parser.parse_args().binary.resolve())
