"""Disposable Docker Engine protocol peer; no Docker service, container or CLI used."""
import http.server
import json
import socketserver
import threading
import urllib.parse

A, B, IMAGE = 'a' * 64, 'b' * 64, 'sha256:' + 'c' * 64

def labels(replica=1, **changes):
    result = {'com.docker.compose.project':'shop', 'com.docker.compose.service':'api',
              'com.docker.compose.container-number':str(replica), 'com.docker.compose.oneoff':'False'}
    result.update(changes)
    return result

class Fixture(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True
    def __init__(self, path):
        super().__init__(str(path), Handler)
        self.rows = [self.row(A)]
        self.calls, self.execs, self.errors = [], {}, []
        self.mode = ''
        self.entered, self.release, self.disconnected = threading.Event(), threading.Event(), threading.Event()
    @staticmethod
    def row(identity, replica=1):
        return {'Id':identity,'State':'running','Labels':labels(replica)}
    def handle_error(self, *_):
        import traceback
        self.errors.append(traceback.format_exc())

class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *_): pass
    def reply(self, body, status=200):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header('Content-Length', str(len(data)))
        self.send_header('Connection', 'close')
        try:
            self.end_headers(); self.wfile.write(data)
        except (BrokenPipeError,ConnectionResetError):
            # Rejection/cancellation intentionally closes a response before it is consumed.
            self.close_connection = True
    def record(self, body=None):
        self.server.calls.append((self.command,self.path,body))
    def do_GET(self):
        self.record()
        path = urllib.parse.urlsplit(self.path).path
        state = self.server
        if path == '/version': return self.reply({'ApiVersion':'1.47','MinAPIVersion':'1.24'})
        if path == '/v1.45/containers/json':
            query = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
            filters = json.loads(query['filters'][0])
            assert filters['status'] == ['running'] and 'com.docker.compose.project=shop' in filters['label']
            assert 'com.docker.compose.service=api' in filters['label'] and 'com.docker.compose.oneoff=False' in filters['label']
            if state.mode == 'metadata': return self.reply({'unexpected':'object'})
            if state.mode == 'oversize': return self.reply(['x' * (1024*1024+1)])
            return self.reply(state.rows)  # Deliberately ignore filters: caller must verify.
        if path.startswith('/v1.45/containers/') and path.endswith('/json'):
            identity = path.split('/')[-2]
            row = next((r for r in state.rows if r['Id']==identity),None)
            if row is None or state.mode == 'vanished': return self.reply({},404)
            observed_labels = row['Labels'] if state.mode != 'relabelled' else labels(**{'com.docker.compose.service':'other'})
            return self.reply({'Id':B if state.mode=='identity' else identity, 'Image':IMAGE,
                'State':{'Running':state.mode!='stopped','Paused':state.mode=='paused'}, 'Config':{'Labels':observed_labels}})
        if path.startswith('/v1.45/exec/') and path.endswith('/json'):
            identity = path.split('/')[-2]; instance = state.execs[identity]
            return self.reply({'ID':identity,'ContainerID':instance['container'], 'Running':instance['running'], 'ExitCode':instance['exit']})
        self.reply({},404)
    def do_POST(self):
        state = self.server
        length = int(self.headers.get('Content-Length','0')); assert length <= 2*1024*1024
        body = json.loads(self.rfile.read(length)) if length else None
        self.record(body)
        path = urllib.parse.urlsplit(self.path).path
        if path.startswith('/v1.45/containers/') and path.endswith('/exec'):
            identity = path.split('/')[-2]
            assert identity in (A,B), identity
            assert body['Privileged'] is False
            if body['Tty']:
                assert body['AttachStdin'] is True and body['Cmd']==['/bin/sh','-i']
                assert body['ConsoleSize']==[24,80]
                assert not any(v.startswith(('WES_','HOME=','PATH=','ZDOTDIR=')) for v in body['Env'])
            else:
                assert body['AttachStdin'] is False and body['Cmd'][0]=='/bin/echo'
            exec_id = format(len(state.execs)+1,'064x')
            state.execs[exec_id] = {'container':identity,'config':body,'running':True,'exit':0}
            if state.mode == 'pause_create':
                state.entered.set(); assert state.release.wait(10)
            return self.reply({'Id':exec_id},201)
        if path.startswith('/v1.45/exec/') and path.endswith('/resize'):
            assert urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query) == {'h':['31'],'w':['101']}
            return self.reply({})
        if path.startswith('/v1.45/exec/') and path.endswith('/start'):
            identity = path.split('/')[-2]; instance = state.execs[identity]
            if state.mode == 'reject_start': return self.reply({},500)
            if state.mode == 'hang_start':
                state.entered.set(); self.connection.settimeout(10)
                try: self.rfile.read(1)
                finally: state.disconnected.set()
                return
            if not body['Tty']:
                output = (instance['config']['Cmd'][1]+'\n').encode()
                data = bytes([1,0,0,0])+len(output).to_bytes(4,'big')+output
                instance.update(running=False,exit=0)
                self.send_response(200); self.send_header('Content-Length',str(len(data))); self.send_header('Connection','close'); self.end_headers(); self.wfile.write(data)
                return
            assert self.headers['Upgrade']=='tcp' and body['ConsoleSize']==[24,80]
            self.close_connection = True  # Ownership moved to the raw session; never parse another HTTP request.
            # First raw bytes share the header write: preserve upgrade read-ahead.
            self.wfile.write(b'HTTP/1.1 101 UPGRADED\r\nConnection: Upgrade\r\nUpgrade: tcp\r\nContent-Type: application/vnd.docker.raw-stream\r\n\r\n\x1b[32mREADY\x1b[0m\r\n'); self.wfile.flush()
            self.connection.settimeout(15)
            try:
                if state.mode == 'disconnect': return
                while True:
                    byte = self.rfile.read(1)
                    if not byte: return
                    if byte == b'\x04':
                        instance.update(running=False,exit=7); return
                    self.wfile.write(byte); self.wfile.flush()
            except (BrokenPipeError,ConnectionResetError): pass
            finally: state.disconnected.set()
            return
        self.reply({},404)
