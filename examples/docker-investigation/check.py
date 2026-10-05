#!/usr/bin/env python3
"""Isolated actual application/PTY/MCP, synthetic Docker socket; never contacts user daemons."""
import argparse, base64, http.server, json, os, selectors, shlex, socketserver, struct
import subprocess, tempfile, threading, time, urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
ID = 'a'*64
IMAGE = 'sha256:'+'b'*64
LINES = ['ERROR allocation failed' if i < 12 else 'INFO synthetic request completed '+str(i)+' payload='+'x'*96 for i in range(200)]

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_GET(self):
        self.server.calls.append(self.path)
        if self.path == '/version':
            data = dict(ApiVersion='1.54', MinAPIVersion='1.40')
        elif self.path.startswith('/v1.45/containers/json?'):
            data = [dict(Id=ID, Names=['/fixture-api'], ImageID=IMAGE, State='restarting', Created=1,
                         Labels={'com.docker.compose.project':'wes-investigation'})]
        elif self.path == f'/v1.45/containers/{ID}/json':
            data = dict(Id=ID, Name='/fixture-api', Image=IMAGE, Created='2026-09-25T00:00:00Z',
                        State=dict(Status='restarting', Running=True, ExitCode=137, OOMKilled=True),
                        RestartCount=7, Config=dict(Tty=False, Env=['SECRET=must-not-export']))
        elif self.path.startswith(f'/v1.45/containers/{ID}/logs?'):
            body = b''
            for line in LINES:
                payload = ('2026-09-25T00:00:00.123456789Z '+line+'\n').encode()
                body += struct.pack('>BxxxI', 2 if line.startswith('ERROR') else 1, len(payload))+payload
            data = None
        else:
            self.send_error(404); return
        if data is not None: body = json.dumps(data).encode()
        self.send_response(200); self.send_header('Content-Length', str(len(body))); self.end_headers()
        self.wfile.write(body)

class Daemon(socketserver.ThreadingUnixStreamServer):
    daemon_threads = True

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--report', type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix='wes-docker-agent-', dir='/tmp') as folder:
        root = Path(folder)
        daemon = Daemon(str(root/'docker.sock'), Handler); daemon.calls = []
        thread = threading.Thread(target=daemon.serve_forever, daemon=True); thread.start()
        env = {k:v for k,v in os.environ.items() if not any(w in k for w in ['TOKEN','SECRET','PASSWORD','API_KEY'])}
        env['HOME'] = str(root)
        with (root/'server.log').open('w+') as errors:
            app = subprocess.Popen([str(binary),'--home',str(root/'home'),'--serve','0','--no-auto-keep'],
                cwd=root, env=env, stdout=subprocess.PIPE, stderr=errors, text=True)
            events = None
            try:
                with selectors.DefaultSelector() as select:
                    select.register(app.stdout, selectors.EVENT_READ); assert select.select(30), 'startup timeout'
                url = app.stdout.readline().strip().removeprefix('Listening at ')
                events = urllib.request.urlopen(url+'/events', timeout=20)
                while True:
                    line = events.readline().decode(); assert line, 'closed events'
                    if line.startswith('data:'):
                        event = json.loads(line[5:])
                        if event['event'] == 'session': generation = event['generation']; break
                def terminal(action, **kwargs):
                    data = dict(client='synthetic-user', action=action, **kwargs)
                    request = urllib.request.Request(url+'/terminals', json.dumps(data).encode(),
                        {'Content-Type':'application/json','X-Wes-Session':generation})
                    with urllib.request.urlopen(request, timeout=20) as response: return json.load(response)
                identity = terminal('start')['id']
                terminal('write', id=identity, text='python3 '+shlex.quote(str(HERE/'agent.py'))+' '+shlex.quote(str(root))+'\r')
                output = ''; cursor = 0; end = time.monotonic()+90; failure = None
                while time.monotonic() < end:
                    frame = terminal('poll', id=identity, cursor=cursor, wait_ms=20)
                    cursor = frame['next']; output += base64.b64decode(frame['data']).decode('utf-8','replace')
                    if 'Traceback (most recent call last)' in output:
                        if failure is None: failure = time.monotonic()
                        assert time.monotonic()-failure < 1, output
                    if 'DOCKER_INVESTIGATION_OK\r\n' in output: break
                assert 'DOCKER_INVESTIGATION_OK\r\n' in output, output
                terminal('close', id=identity)
                reports = list((root/'mcp-reports').glob('mcp-*.json'))
                assert len(reports) == 1, reports
                metrics = json.loads(reports[0].read_text()); assert metrics['state'] == 'closed', metrics
                report = json.loads((root/'measurement.json').read_text())
                report['daemon_requests'] = daemon.calls
                report['mcp_metrics'] = metrics
                # Native: negotiation, inventory, inspection, TTY inspection, logs. Each shell: four.
                assert len(daemon.calls) == 13, daemon.calls
                if args.report: args.report.write_text(json.dumps(report, indent=2)+'\n')
                print(json.dumps(report['costs'], indent=2))
                print('PASS: native and shell investigations agree; actual MCP wire bytes and closed metrics report; 13 bounded daemon reads')
            finally:
                if events: events.close()
                app.terminate()
                try: app.wait(timeout=15)
                except subprocess.TimeoutExpired: app.kill(); app.wait()
                daemon.shutdown(); daemon.server_close(); thread.join()

if __name__ == '__main__': main()
