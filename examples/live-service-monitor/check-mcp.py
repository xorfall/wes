#!/usr/bin/env python3
"""Run actual monitor files through two MCP connections; no model, browser or user data."""
import argparse, base64, json, os, selectors, shlex, subprocess, tempfile, time, urllib.request
from pathlib import Path
from fixture import DemoServer, sample
from support import copy_project, ROOT, HERE

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,default=ROOT/'target/debug/wes')
    binary=parser.parse_args().binary.resolve()
    server=DemoServer()
    try:
        with tempfile.TemporaryDirectory(prefix='wes-monitor-mcp-') as directory:
            root=Path(directory);copy_project(root,server.server_port)
            def signal(name):
                pending=root/(name+'.tmp');pending.write_text('{}');pending.replace(root/name)
            env={k:v for k,v in os.environ.items() if not any(w in k for w in ['API_KEY','TOKEN','SECRET','PASSWORD'])}
            env['HOME']=str(root)
            with (root/'server.log').open('w+') as errors:
                app=subprocess.Popen([str(binary),'--home',str(root/'home'),'--serve','0','--no-auto-keep'],cwd=root,env=env,stdout=subprocess.PIPE,stderr=errors,text=True)
                events=None
                try:
                    with selectors.DefaultSelector() as selector:
                        selector.register(app.stdout,selectors.EVENT_READ);assert selector.select(30),'startup timeout'
                    url=app.stdout.readline().strip().removeprefix('Listening at ')
                    events=urllib.request.urlopen(url+'/events',timeout=15)
                    generation=None
                    while not generation:
                        line=events.readline().decode()
                        if line.startswith('data:'):
                            event=json.loads(line[5:])
                            if event['event']=='session':generation=event['generation']
                    def post(path,body):
                        request=urllib.request.Request(url+path,json.dumps(body).encode(),{'Content-Type':'application/json','X-Wes-Session':generation})
                        with urllib.request.urlopen(request,timeout=15) as reply:
                            data=reply.read();return json.loads(data) if data else None
                    post('/submit',{'request':'submit','client':'synthetic-user','cell':'seed','text':':calc { return 41; } > user_seed'})
                    while True:
                        line=events.readline().decode()
                        if line.startswith('data:') and json.loads(line[5:])['event']=='ready':break
                    def terminal(action,**kwargs):return post('/terminals',{'client':'synthetic-user','action':action,**kwargs})
                    identities={role:terminal('start')['id'] for role in ['a','b']}
                    cursors=dict.fromkeys(identities,0);output=dict.fromkeys(identities,'')
                    for role,identity in identities.items():
                        command='python3 '+shlex.quote(str(HERE/'mcp_agent.py'))+' '+role+' '+shlex.quote(str(root))+'\r'
                        terminal('write',id=identity,text=command)
                    published=0;granted=False;revoked=False;closed=False;error_since=None
                    end=time.monotonic()+90
                    while time.monotonic()<end:
                        for role,identity in identities.items():
                            if role=='a' and closed:continue
                            frame=terminal('poll',id=identity,cursor=cursors[role],wait_ms=20)
                            cursors[role]=frame['next'];output[role]+=base64.b64decode(frame['data']).decode('utf-8','replace')
                        if any('Traceback (most recent call last)' in text for text in output.values()):
                            if error_since is None:error_since=time.monotonic()
                            assert time.monotonic()-error_since<1,output
                        if (root/'phase.json').exists():
                            last=json.loads((root/'phase.json').read_text())['last']
                            if last>published:
                                server.publish([sample(n) for n in range(published+1,last+1)])
                                published=last;signal(f'published-{last}.json')
                        if not granted and (root/'needs-grant.json').exists():
                            assert len(server.requests)==1 and server.active==1,server.requests
                            actor=json.loads((root/'a-ready.json').read_text())['actor']
                            peer=json.loads((root/'peer.json').read_text())['cell']
                            post('/submit',{'request':'work-grant','actor':actor,'cells':[peer]})
                            signal('granted.json');granted=True
                        if not revoked and (root/'cancelled.json').exists():
                            with server.changed:
                                assert server.changed.wait_for(lambda: server.active == 0, 10), 'MCP cancel left the SSE subscription open'
                            post('/submit',{'request':'work-grant','actor':actor,'cells':[]})
                            signal('revoked.json');revoked=True
                        if not closed and 'COOPERATIVE_A_OK\r\n' in output['a']:
                            assert len(server.requests)==2,server.requests
                            terminal('close',id=identities['a']);closed=True
                            signal('a-closed.json')
                        if 'COOPERATIVE_B_OK\r\n' in output['b']:break
                    assert granted and revoked and closed and 'COOPERATIVE_B_OK\r\n' in output['b'],output
                    a=json.loads((root/'a-identity.json').read_text());b=json.loads((root/'b-identity.json').read_text())
                    assert a['actor']!=b['actor']
                    print(json.dumps(json.loads((root/'a-ready.json').read_text())['setup']))
                    print('PASS: actual monitor via MCP, bounded stream, reactive views, two actors, exact grant/revoke, protected user work, one subscription per request, stream survives agent departure')
                    terminal('close',id=identities['b'])
                finally:
                    if events:events.close()
                    app.terminate()
                    try:app.wait(timeout=15)
                    except subprocess.TimeoutExpired:app.kill();app.wait()
    finally:server.close()
if __name__=='__main__':main()
