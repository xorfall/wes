#!/usr/bin/env python3
"""Exercise the actual spec, environment and scripts with synthetic loopback responses."""
import argparse, json, subprocess, tempfile
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Thread
HERE = Path(__file__).resolve().parent
class Handler(BaseHTTPRequestHandler):
    requests = []
    def log_message(self, *_): pass
    def do_GET(self):
        self.requests.append(self.path)
        status, body = {'/ok': (200, b'{"title":"Synthetic item"}'), '/bad': (400, b'{"message":"Synthetic rejection"}'), '/mismatch': (200, b'{"title":42}'), '/empty': (204, b'')}[self.path]
        self.send_response(status)
        if body: self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers(); self.wfile.write(body)
def values(output):
    result = []
    for line in output.splitlines():
        if ': ' in line:
            try: result.append(json.loads(line.partition(': ')[2]))
            except json.JSONDecodeError: pass
    return result
def main():
    parser=argparse.ArgumentParser(); parser.add_argument('--binary', type=Path, default=HERE.parents[1]/'target/debug/wes')
    binary=parser.parse_args().binary.resolve()
    server=ThreadingHTTPServer(('127.0.0.1',0),Handler)
    thread=Thread(target=server.serve_forever,daemon=True); thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='wes-http-responses-') as directory:
            root=Path(directory)
            for name in ['api.json','environments.yaml','demo.wes','offline.wes']:
                (root/name).write_text((HERE/name).read_text().replace('http://127.0.0.1:8768',f'http://127.0.0.1:{server.server_port}'))
            def run(*args):
                p=subprocess.run([str(binary),'--home',str(root/'home'),*args],cwd=root,text=True,capture_output=True,timeout=45)
                assert p.returncode==0,p.stdout+p.stderr
                return values(p.stdout)
            outputs=run('--env-file','environments.yaml','--env','demo','--activate-env','--file','demo.wes')
            assert {'status':200,'title':'Synthetic item'} in outputs, outputs
            assert {'status':400,'message':'Synthetic rejection'} in outputs, outputs
            assert {'state':'mismatch','received':42} in outputs, outputs
            assert {'status':204,'kind':'empty'} in outputs, outputs
            assert 'unexpected invocation failure' not in outputs, outputs
            assert sorted(Handler.requests) == ['/bad','/empty','/mismatch','/ok'],Handler.requests
            server.shutdown();server.server_close();thread.join()
            outputs=run('--file','offline.wes')
            assert {'status':400,'message':'Synthetic rejection'} in outputs, outputs
            assert len(Handler.requests)==4
    finally:
        server.shutdown();server.server_close();thread.join()
    print('PASS HTTP envelopes: actual spec, typed body, received 400 success branch, mismatch evidence, empty body and offline replay')
if __name__=='__main__': main()
