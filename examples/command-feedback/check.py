#!/usr/bin/env python3
"""Run the command feedback through a real isolated PTY/MCP connection."""
import argparse, base64, json, os, selectors, shlex, subprocess, tempfile, time, urllib.request, threading
from pathlib import Path
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', action='store_true')
    parser.add_argument('--binary', type=Path, default=ROOT/'target/debug/wes')
    args=parser.parse_args()
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix='wes-feedback-') as directory:
        root = Path(directory)
        env = {k:v for k,v in os.environ.items() if not any(w in k for w in ['TOKEN','SECRET','PASSWORD','API_KEY'])}
        with (root/'server.log').open('w+') as errors:
            app = subprocess.Popen([str(binary),'--home',str(root/'home'),'--serve','0','--no-auto-keep'],
                                   cwd=root,env=env,stdout=subprocess.PIPE,stderr=errors,text=True)
            events = None
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(app.stdout,selectors.EVENT_READ);assert selector.select(30),'startup timeout'
                url = app.stdout.readline().strip().removeprefix('Listening at ')
                events = urllib.request.urlopen(url+'/events',timeout=120)
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
                post('/submit',{'request':'submit','client':'synthetic-user','cell':'human-failed','text':':env use private_environment'})
                until(lambda e:e['event']=='reported' and e.get('cell')=='human-failed')
                reports={}
                report_changes=threading.Condition()
                report_errors=[]
                def collect():
                    try:
                        while True:
                            event=until(lambda e:e['event']=='reported')
                            with report_changes:
                                reports[event['cell']]=event
                                report_changes.notify_all()
                    except (ValueError,OSError,AssertionError) as error:
                        with report_changes:
                            report_errors.append(str(error))
                            report_changes.notify_all()
                threading.Thread(target=collect,daemon=True).start()
                def terminal(action,**kwargs):return post('/terminals',{'client':'synthetic-user','action':action,**kwargs})
                identity=terminal('start')['id']
                terminal('write',id=identity,text='python3 '+shlex.quote(str(HERE/'agent.py'))+' '+shlex.quote(str(root))+(' --baseline' if args.baseline else '')+'\r')
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
                    if 'FEEDBACK_OK\r\n' in output:break
                assert granted and revoked and 'FEEDBACK_OK\r\n' in output,output
                terminal('close',id=identity)
                records=json.loads((root/'records.json').read_text())
                if not args.baseline:
                    for record in records:
                        execution=record['reply']
                        if execution['diagnostics'] and record['source'].startswith(':env') and any(d['severity']=='error' for d in execution['diagnostics']):
                            with report_changes:
                                arrived=report_changes.wait_for(lambda:execution['cell'] in reports or report_errors,timeout=5)
                                assert arrived and execution['cell'] in reports, (record['source'],report_errors,list(reports))
                                ui=reports[execution['cell']]['diagnostics']
                            assert [(d['code'],d['severity'],d['message']) for d in ui]==[(d['code'],d['severity'],d['message']) for d in execution['diagnostics']], (ui,execution)
                    # CLI rendering is independent of MCP rendering.
                    probe=subprocess.run([str(binary),'--home',str(root/'cli'),'--command',':env use claude_dev'],cwd=root,env=env,text=True,capture_output=True)
                    assert probe.returncode==1 and 'Unexpected command path word.' in probe.stderr, probe
                print(json.dumps([{'case':i,'bytes':r['bytes'],'calls':r['calls'],'message':r.get('error') or [d['message'] for d in r['reply']['diagnostics']]} for i,r in enumerate(records)]))
                print('PASS: command feedback, UI/CLI/MCP causes, source grants, bounded stress, corrected retry')
            finally:
                app.terminate()
                try:app.wait(timeout=15)
                except subprocess.TimeoutExpired:app.kill();app.wait()
                if events:events.close()

if __name__=='__main__':main()
