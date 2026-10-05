#!/usr/bin/env python3
"""Real native server/PTY boundary; all commands and data use a disposable home."""
import argparse, importlib.util, json, tempfile, uuid, urllib.request, urllib.error
from pathlib import Path
source = Path(__file__).resolve().parents[1] / 'terminal' / 'check-history.py'
spec = importlib.util.spec_from_file_location('terminal_fixture', source)
fixture = importlib.util.module_from_spec(spec); spec.loader.exec_module(fixture)

def check(binary):
    with tempfile.TemporaryDirectory(prefix='wes-delete-example-') as directory:
        root = Path(directory); (root/'user').mkdir()
        server = fixture.Server(binary, root, root/'data')
        def request(path, body):
            req = urllib.request.Request(server.url + path, json.dumps(body).encode(),
                {'Content-Type':'application/json','X-Wes-Session':server.generation})
            with urllib.request.urlopen(req, timeout=15) as response: return json.load(response)
        try:
            key = str(uuid.uuid4()); terminal = server.start(key)
            server.call('write', id=terminal, text=': SYNTHETIC_DELETE_HISTORY\r'); server.output(terminal)
            files = list((root/'data').rglob('*'))
            histories = [p for p in files if p.is_file() and b'SYNTHETIC_DELETE_HISTORY' in p.read_bytes()]
            assert histories, 'Synthetic terminal command was not retained'
            before = {p:p.read_bytes() for p in histories}
            preview = request('/workspace-deletion', {'action':'preview','client':'delete-fixture'})
            assert terminal in preview['terminals'] and not preview['blockers'], preview
            server.call('poll', id=terminal, cursor=0, wait_ms=0) # Preview did not stop it.
            try:
                request('/workspace-deletion', {'action':'confirm','client':'delete-fixture','token':preview['token'],'stop':False,'protected':False})
                raise AssertionError('Active terminal deleted without Stop approval')
            except urllib.error.HTTPError as error: assert error.code == 409
            preview = request('/workspace-deletion', {'action':'preview','client':'delete-fixture'})
            result = request('/workspace-deletion', {'action':'confirm','client':'delete-fixture','token':preview['token'],'stop':True,'protected':False})
            assert result['deleted'], result
            for path, data in before.items(): assert path.read_bytes() == data, 'Independent terminal history removed'
            opened = request('/workspaces', {'name':preview['workspace'],'create':True})
            assert opened['identity'] != preview['identity'] and opened['generation'] != server.generation
            print('PASS: pure preview, explicit joined terminal stop, preserved pane history, fresh recreated identity')
        finally: server.close()

if __name__ == '__main__':
    parser = argparse.ArgumentParser(); parser.add_argument('--binary', type=Path, required=True)
    check(parser.parse_args().binary.resolve())
