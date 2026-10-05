#!/usr/bin/env python3
"""Actual recipe/source through isolated application + synthetic Docker socket."""
import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import selectors
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid
from fixture import Fixture, A, B

HERE = Path(__file__).resolve().parent

class App:
    def __init__(self, binary, root, recipe):
        self.events = []
        env = dict(os.environ, HOME=str(root))
        path = root/'environments.yaml'; path.write_text(json.dumps(recipe))
        result = subprocess.run([str(binary),'--home',str(root/'home'),'--env-file',str(path),'--command',':calc { return 0; }'],cwd=root,env=env,capture_output=True,text=True,timeout=30)
        assert result.returncode==0,result.stderr
        self.log = (root/'app.log').open('w+')
        self.process = subprocess.Popen([str(binary),'--home',str(root/'home'),'--serve','0'],cwd=root,env=env,stdout=subprocess.PIPE,stderr=self.log,text=True)
        with selectors.DefaultSelector() as select:
            select.register(self.process.stdout,selectors.EVENT_READ); assert select.select(20),'startup timeout'
        self.url = self.process.stdout.readline().strip().removeprefix('Listening at ')
        self.stream = urllib.request.urlopen(self.url+'/events',timeout=20)
        self.generation = self.until(lambda e:e['event']=='session')['generation']
        self.source(':env use "compose_demo"')
        self.source(':env enable "compose_demo"')
    def until(self,predicate,after=0):
        for event in self.events[after:]:
            if predicate(event): return event
        while True:
            line = self.stream.readline(); assert line,'closed event stream'
            if not line.startswith(b'data:'): continue
            event=json.loads(line[5:]); self.events.append(event)
            if predicate(event): return event
    def post(self,path,body):
        request=urllib.request.Request(self.url+path,json.dumps(body).encode(),{'Content-Type':'application/json','X-Wes-Session':self.generation})
        with urllib.request.urlopen(request,timeout=25) as response:
            data=response.read(); return json.loads(data) if data else None
    def source(self,text):
        cell=str(uuid.uuid4()); after=len(self.events)
        self.post('/submit',{'request':'submit','client':'docker-qa','cell':cell,'text':text})
        report=self.until(lambda e:e['event']=='reported' and e.get('cell')==cell,after)
        assert not any(d.get('severity')=='error' for d in report.get('diagnostics',[])),report
        return after
    def value(self,name,after,previous=None):
        created=self.until(lambda e:e['event']=='created' and e.get('name')==name)
        event=self.until(lambda e:e.get('node')==created['node'] and e['event'] in ('ready','failed') and (e['event']=='failed' or e.get('handle')!=previous),after)
        assert event['event']=='ready',event
        with urllib.request.urlopen(self.url+'/values/'+event['handle'],timeout=10) as response: value=json.load(response)
        return value,event['handle']
    def terminal(self,action,**fields): return self.post('/terminals',{'action':action,'client':'docker-qa',**fields})
    def target(self,name):
        choice=next(c for c in self.terminal('targets')['targets'] if c['target']==name)
        assert choice['available'],choice
        return {key:choice[key] for key in ('environment','revision','target')}
    def start(self,target):
        try: return self.terminal('start',target=target)
        except urllib.error.HTTPError as error:
            assert error.code==400,error.code
            return {'error':error.read().decode()}
    def close(self):
        self.stream.close(); self.process.terminate()
        try: self.process.wait(timeout=10)
        except subprocess.TimeoutExpired: self.process.kill(); self.process.wait(timeout=5)
        self.log.close()

def run(binary,root,daemon,recipe):
    app=App(binary,root,recipe)
    try:
        target,replica,fixed=(app.target(name) for name in ('api','api_replica','fixed'))
        before = len(daemon.calls)
        try: app.terminal('resolve')
        except urllib.error.HTTPError as error:
            assert error.code == 400 and 'specify target:NAME' in error.read().decode()
        else: raise AssertionError('multiple targets must require target:')
        assert app.terminal('resolve', target='api')['target'] == target
        assert app.terminal('resolve', environment='compose_demo', target='api')['target'] == target
        assert len(daemon.calls) == before, 'context resolution must not contact Docker'
        # A changed definition yields redacted evidence, without opening/contacting a target.
        app.source(':import process bin:/bin/echo as:review_echo target:api')
        before = len(daemon.calls)
        reviewed = app.start(target)
        assert 'id' not in reviewed and reviewed['review']['previousRevision'] == target['revision'], reviewed
        assert reviewed['review']['changes']['added'] == ['review_echo'], reviewed
        assert len(daemon.calls) == before, 'review must not contact Docker'
        target = reviewed['review']['target']
        # Approval of an intermediate revision is still only another review.
        app.source(':env use "compose_demo"')
        app.source(':import process bin:/bin/echo as:review_second target:api')
        before = len(daemon.calls)
        reviewed = app.start(target)
        assert 'id' not in reviewed and reviewed['review']['previousRevision'] == target['revision'], reviewed
        assert reviewed['review']['changes']['added'] == ['review_second'], reviewed
        assert len(daemon.calls) == before, 'superseded approval must not contact Docker'
        target = reviewed['review']['target']
        replica, fixed = app.target('api_replica'), app.target('fixed')
        app.source(':env use "compose_demo"')
        # Same node, immutable old value evidence, a fresh explicit run resolves once again.
        after=app.source((HERE/'run.wes').read_text()); old,handle=app.value('result',after)
        assert old['provenance']['docker.container']==A,old
        assert old['provenance']['docker.compose']=='shop/api',old
        assert base64.b64decode(old['data']['stdout']).decode()=="literal ; $(touch SHOULD_NOT_EXIST) $HOME 'quote'\n"
        app.post('/submit',{'request':'keep','handle':handle})
        daemon.rows=[daemon.row(B)]
        after=app.source((HERE/'refresh.wes').read_text()); new,_=app.value('result',after,handle)
        assert new['provenance']['docker.container']==B,new
        with urllib.request.urlopen(app.url+'/values/'+handle,timeout=10) as response: retained=json.load(response)
        assert retained['provenance']['docker.container']==A
        assert 'ENV032' in app.start(fixed)['error']
        # Resolve once, input is raw and duplex, final exit belongs to the same exec/container.
        opened=app.start(target); assert 'error' not in opened,opened
        identity=opened['id']; cursor=0
        assert opened['destination']=='Docker container '+B and opened['workspace_tools'] is False and opened['cwd'] is None
        def poll_until(marker=None,closed=False):
            nonlocal cursor
            output=b''; deadline=time.monotonic()+10
            while time.monotonic()<deadline:
                frame=app.terminal('poll',id=identity,cursor=cursor,wait_ms=100)
                cursor=frame['next']; output+=base64.b64decode(frame['data'])
                if (closed and frame['closed']) or (marker and marker in output): return output,frame
            raise AssertionError(('terminal timeout',output,frame))
        poll_until(b'\x1b[32mREADY\x1b[0m\r\n')
        listed=sum('/containers/json?' in path for _,path,_ in daemon.calls)
        daemon.rows=[daemon.row(A)]  # A service replacement never moves an open endpoint.
        app.terminal('write',id=identity,text='çığ漢字\r')
        poll_until('çığ漢字\r'.encode())
        app.terminal('resize',id=identity,cols=101,rows=31)
        assert sum('/containers/json?' in path for _,path,_ in daemon.calls)==listed
        app.terminal('write',id=identity,text='\x04')
        _,frame=poll_until(closed=True)
        assert frame['exit']==7 and frame['problem'] is None,frame
        app.terminal('close',id=identity)
        # No/multiple/one-off/foreign/malformed rows cannot cause an implicit selection.
        for rows,mode,code in [([], '', 'ENV038'),([daemon.row(A),daemon.row(B,2)],'','ENV038'),
            ([{**daemon.row(A),'Labels':{**daemon.row(A)['Labels'],'com.docker.compose.oneoff':'True'}}],'','ENV038'),
            ([daemon.row(A)],'metadata','ENV034'),([daemon.row(A)],'oversize','ENV034'),
            ([daemon.row(A)],'identity','ENV034'),([daemon.row(A)],'relabelled','ENV038'),
            ([daemon.row(A)],'vanished','ENV032'),([daemon.row(A)],'paused','ENV032'),([daemon.row(A)],'stopped','ENV032')]:
            daemon.rows,daemon.mode=rows,mode; before=len(daemon.execs)
            refused=app.start(target); assert code in refused.get('error',''),(mode,refused)
            assert len(daemon.execs)==before
        daemon.rows=[daemon.row(A),daemon.row(B,2)]; daemon.mode=''
        opened=app.start(replica); identity,cursor=opened['id'],0
        assert opened['destination']=='Docker container '+B
        poll_until(b'READY'); daemon.disconnected.clear()
        closed=app.terminal('close',id=identity)
        assert 'ENV036' in closed['problem'] and daemon.disconnected.wait(2),closed
        # Authority loss during setup must not be repaired by a fast re-enable.
        daemon.rows=[daemon.row(A)]; daemon.mode='pause_create'; daemon.entered.clear(); daemon.release.clear()
        with ThreadPoolExecutor(max_workers=1) as executor:
            starting=executor.submit(app.start,target); assert daemon.entered.wait(5)
            before=sum(path.endswith('/start') for _,path,_ in daemon.calls)
            app.source(':env disable "compose_demo"'); app.source(':env enable "compose_demo"')
            daemon.release.set(); refused=starting.result(timeout=10)
            assert 'ENV020' in refused.get('error',''),refused
            assert sum(path.endswith('/start') for _,path,_ in daemon.calls)==before
        # Once start is sent, losing its reply is an uncertain remote outcome.
        daemon.mode='hang_start'; daemon.entered.clear(); daemon.disconnected.clear()
        with ThreadPoolExecutor(max_workers=1) as executor:
            starting=executor.submit(app.start,target); assert daemon.entered.wait(5)
            app.source(':env disable "compose_demo"')
            refused=starting.result(timeout=10)
            assert 'ENV036' in refused.get('error','') and daemon.disconnected.wait(2),refused
        app.source(':env enable "compose_demo"')
        daemon.mode='disconnect'
        opened=app.start(target); identity,cursor=opened['id'],0
        _,frame=poll_until(closed=True); assert 'ENV036' in frame['problem'],frame
        app.terminal('close',id=identity)
        daemon.mode='reject_start'
        refused=app.start(target); assert 'ENV036' in refused.get('error',''),refused
        daemon.mode=''
        opened=app.start(target); identity,cursor=opened['id'],0
        poll_until(b'READY'); daemon.disconnected.clear()
        app.source(':env disable "compose_demo"')
        _,frame=poll_until(closed=True)
        assert 'ENV036' in frame['problem'] and daemon.disconnected.wait(2),frame
        app.terminal('close',id=identity)
        # Only read/exec endpoints are allowed. No lifecycle, shell CLI or automatic retry.
        assert not any('/containers/create' in path or path.endswith(('/kill','/stop','/restart')) for _,path,_ in daemon.calls)
        assert not daemon.errors,daemon.errors
        print('PASS Docker terminal + Compose: actual files, frozen ID and refresh evidence, unique/replica/zero/ambiguous/one-off selection, malformed and changed identity refusal, raw upgrade/input/resize/exit, joined close/disable, cancelled setup and uncertain start; no host tools or retries')
    finally: daemon.release.set(); app.close()

def main():
    parser=argparse.ArgumentParser(description=__doc__); parser.add_argument('--binary',type=Path,required=True)
    args=parser.parse_args()
    with tempfile.TemporaryDirectory(prefix='wes-dexec-',dir='/tmp') as folder:
        root=Path(folder); socket=root/'docker.sock'; daemon=Fixture(socket)
        thread=threading.Thread(target=daemon.serve_forever,daemon=True); thread.start()
        recipe=json.loads((HERE/'environments.yaml').read_text())
        for target in recipe['targets'].values(): target['socket']=str(socket)
        try: run(args.binary.resolve(),root,daemon,recipe)
        finally: daemon.shutdown(); daemon.server_close(); thread.join(timeout=5)

if __name__=='__main__': main()
