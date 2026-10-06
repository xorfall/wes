#!/usr/bin/env python3
"""Run the actual calc files through a real isolated PTY/MCP connection."""
import sys
import argparse, base64, json, os, selectors, shlex, subprocess, tempfile, time, urllib.request
from pathlib import Path
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
from fixture_environment import isolated
ROOT = HERE.parents[1]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT/'target/debug/wes')
    binary = parser.parse_args().binary.resolve()
    with tempfile.TemporaryDirectory(prefix='wes-inspection-') as directory:
        root = Path(directory)
        env = {k:v for k,v in os.environ.items() if not any(w in k for w in ['TOKEN','SECRET','PASSWORD','API_KEY'])}
        env = isolated(root, env)
        with (root/'server.log').open('w+') as errors:
            app = subprocess.Popen([str(binary),'--home',str(root/'home'),'--serve','0','--no-auto-keep'],
                                   cwd=root,env=env,stdout=subprocess.PIPE,stderr=errors,text=True)
            events = None
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(app.stdout,selectors.EVENT_READ);assert selector.select(30),'startup timeout'
                url = app.stdout.readline().strip().removeprefix('Listening at ')
                events = urllib.request.urlopen(url+'/events',timeout=20)
                def until(predicate):
                    while True:
                        line=events.readline().decode();assert line,'closed event stream'
                        if line.startswith('data:'):
                            event=json.loads(line[5:])
                            if predicate(event): return event
                generation=until(lambda e:e['event']=='session')['generation']
                def post(path,body):
                    request=urllib.request.Request(url+path,json.dumps(body).encode(),{'Content-Type':'application/json','X-Wes-Session':generation})
                    with urllib.request.urlopen(request,timeout=20) as response:
                        data=response.read();return json.loads(data) if data else None
                post('/submit',{'request':'submit','client':'synthetic-user','cell':'human-failed','text':':inspect $human_missing'})
                until(lambda e:e['event']=='reported' and e.get('cell')=='human-failed')
                def terminal(action,**kwargs):return post('/terminals',{'client':'synthetic-user','action':action,**kwargs})
                identity=terminal('start')['id']
                terminal('write',id=identity,text='python3 '+shlex.quote(str(HERE/'agent.py'))+' '+shlex.quote(str(root))+'\r')
                def signal(name):
                    temp=root/(name+'.tmp');temp.write_text('{}');temp.replace(root/name)
                cursor=0;output='';granted=False;revoked=False;failure_since=None
                end=time.monotonic()+90
                while time.monotonic()<end:
                    frame=terminal('poll',id=identity,cursor=cursor,wait_ms=20)
                    cursor=frame['next'];output+=base64.b64decode(frame['data']).decode('utf-8','replace')
                    if 'Traceback (most recent call last)' in output:
                        if failure_since is None:failure_since=time.monotonic()
                        assert time.monotonic()-failure_since<1,output
                    if not granted and (root/'identity.json').exists():
                        actor=json.loads((root/'identity.json').read_text())['actor']
                        post('/submit',{'request':'source-grant','actor':actor,'cells':['human-failed']})
                        signal('granted.json');granted=True
                    if not revoked and (root/'read.json').exists():
                        post('/submit',{'request':'source-grant','actor':actor,'cells':[]})
                        signal('revoked.json');revoked=True
                    if 'INSPECTION_OK\r\n' in output:break
                assert granted and revoked and 'INSPECTION_OK\r\n' in output,output
                terminal('close',id=identity)
                print(json.dumps(json.loads((root/'metrics.json').read_text())))
                print('PASS: actual calc files, bounded pages/metadata, no read-created cells, own/shared/revoked source, actionable failures')
            finally:
                if events:events.close()
                app.terminate()
                try:app.wait(timeout=15)
                except subprocess.TimeoutExpired:app.kill();app.wait()

if __name__=='__main__':main()
