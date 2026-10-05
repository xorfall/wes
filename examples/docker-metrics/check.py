#!/usr/bin/env python3
"""Actual recipe/source acceptance, synthetic by default; real uses only its own fixture."""
import argparse
import contextlib
import http.server
import json
from pathlib import Path
import socketserver
import sys
import tempfile
import threading
import time
from urllib.parse import urlsplit, parse_qs
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / 'docker-logs'))
from check import fixtures, command, ID
sys.path.insert(0, str(HERE.parent / 'live-service-monitor'))
from support import Client

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_GET(self):
        path = urlsplit(self.path)
        with self.server.changed: self.server.requests.append(self.path)
        if path.path == '/version':
            body = json.dumps({'ApiVersion':'1.45','MinAPIVersion':'1.24'}).encode()
        elif path.path.endswith('/json'):
            body = json.dumps({'Id':ID}).encode()
        elif path.path.endswith('/stats') or path.path.endswith('/events'):
            stats = path.path.endswith('/stats')
            if stats: assert parse_qs(path.query)['stream'] == ['true']
            else: assert json.loads(parse_qs(path.query)['filters'][0]) == {'type':['container'],'container':[ID]}
            self.send_response(200); self.send_header('Connection','close'); self.end_headers()
            with self.server.changed:
                self.server.active += 1; self.server.changed.notify_all()
            try:
                for n in range(1, 521):
                    sample = ({'id':ID, 'read':'2026-09-24T00:00:01Z', 'preread':'2026-09-24T00:00:00Z',
                               'cpu_stats':{'cpu_usage':{'total_usage':200},'system_cpu_usage':2000,'online_cpus':4},
                               'precpu_stats':{'cpu_usage':{'total_usage':100},'system_cpu_usage':1000},
                               'memory_stats':{'usage':1000,'limit':2000,'stats':{'inactive_file':200}}} if stats else
                              {'Type':'container','Action':'future_action','Actor':{'ID':ID,'Attributes':{'secret':'DO_NOT_EXPORT'}},'timeNano':n})
                    wire = (json.dumps(sample)+'\n').encode()
                    self.wfile.write(wire)
                self.wfile.flush()
                self.connection.settimeout(20); self.connection.recv(1)
            except (OSError, TimeoutError): pass
            finally:
                with self.server.changed:
                    self.server.active -= 1; self.server.changed.notify_all()
            return
        else:
            self.send_error(404); return
        self.send_response(200); self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
class Server(socketserver.ThreadingMixIn,socketserver.UnixStreamServer): daemon_threads=True
@contextlib.contextmanager
def synthetic(root):
    server=Server(str(root/'d.sock'),Handler)
    server.requests=[];server.active=0;server.changed=threading.Condition()
    thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    try: yield server
    finally: server.shutdown();server.server_close();thread.join()

def main():
    p=argparse.ArgumentParser();p.add_argument('binary',type=Path);p.add_argument('--real',action='store_true');p.add_argument('--socket',default='/var/run/docker.sock');args=p.parse_args()
    with contextlib.ExitStack() as stack:
        root=Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='wes-dmetrics-',dir='/tmp')))
        if args.real:
            owned=stack.enter_context(fixtures(root,True,args.socket,follow=True))
            socket,container,_,_=owned[0];server=None
        else:
            server=stack.enter_context(synthetic(root));socket=str(root/'d.sock');container=ID
        for file in HERE.iterdir():
            if file.suffix in ['.yaml','.wes']: (root/file.name).write_text(file.read_text().replace('SOCKET_PATH',json.dumps(socket)).replace('CONTAINER_ID',container))
        client=Client(args.binary.resolve(),root,environment='observe')
        try:
            client.submit(':env plan file:environments.yaml > plan');client.submit(':env apply $plan');client.wait(lambda:client.context)
            client.submit(':env use "observe"');client.submit(':workspace policy mode:reactive');client.file('observe.wes')
            if args.real:
                # Only the uniquely named container made by our own context manager. Never user work.
                # stats opening proves admitted connection; events may flush headers only on an event.
                client.value('stats',lambda rows:len(rows)>0)
                for _ in range(3):
                    command('docker','--host','unix://'+socket,'pause',container)
                    command('docker','--host','unix://'+socket,'unpause',container)
                    time.sleep(.3)
            required=1 if args.real else 300
            stats=client.value('stats',lambda rows:len(rows)>=required and (rows[-1]['cpu_percent'].get('kind') == 'some' if args.real else rows[-1]['sequence']==520))['data']
            events=client.value('events',lambda rows:len(rows)>=required)['data']
            summary=client.value('resource_summary',lambda s:s['samples']>=required and (not args.real or s['cpu'].get('kind') == 'some'))['data']
            event_summary=client.value('event_summary',lambda s:s['events']>=required)['data']
            assert all(r['container']==container for r in stats+events)
            assert all(r['type']=='container' for r in events)
            assert 'DO_NOT_EXPORT' not in json.dumps(events)
            assert len(json.dumps(summary))+len(json.dumps(event_summary))<1500
            if server:
                assert 300<=len(stats)<=500 and len(events)==500
                assert stats[0]['sequence']==521-len(stats) and events[0]['sequence']==21
                assert float(summary['cpu']['value'])==40 and float(summary['memory']['value'])==40
                assert event_summary['action']=='future_action' and len(server.requests)==4,server.requests
            else:
                assert any(r['action'] in ['pause','unpause'] for r in events)
            for name in ['stats','events']:client.file('cancel-'+name+'.wes')
            for name in ['stats','events','resource_summary','event_summary']:
                client.wait(lambda name=name:client.ready.get(client.names[name],{}).get('event')=='stopped')
                client.value(name)
            if server:
                with server.changed:assert server.changed.wait_for(lambda:server.active==0,5)
            print(f'PASS: actual stats/events/summary/cancel files; bounded windows, small summaries, stopped values; mode={"real" if args.real else "synthetic"}')
        finally:client.close()
if __name__=='__main__':main()
