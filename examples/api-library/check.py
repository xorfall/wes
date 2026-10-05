#!/usr/bin/env python3
"""Actual CLI -> library -> read-only repo/Go extractor -> pinned environment -> HTTP."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from threading import Thread

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE.parent / 'api-import'))
from server import make_server

KEY = {'service': 'inventory', 'apiVersion': 'v1', 'scope': 'default'}


def run(command, *, cwd, expected=0, source=None):
    result = subprocess.run([str(p) for p in command], cwd=cwd, input=source,
                            text=True, capture_output=True, timeout=180)
    assert result.returncode == expected, result.stdout + result.stderr
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/wes')
    args = parser.parse_args()
    binary = args.binary.resolve()
    server = make_server(0)
    server.spec = json.loads((HERE / 'repository/source.openapi.json').read_text())
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='wes-api-library-') as temporary:
            tmp = Path(temporary).resolve()
            home = tmp / 'home'
            library = home / 'api-library'
            extractor = tmp / 'wes-extract'
            run(['go', 'build', '-o', extractor, './cmd/extract'], cwd=ROOT / 'tools/describe')
            repo = tmp / 'repository'
            shutil.copytree(HERE / 'repository', repo)
            original = {p.relative_to(repo): p.read_bytes() for p in repo.rglob('*') if p.is_file()}

            def action(value, expected=0):
                result = run([binary, '--home', home, '--api-request', '-'], cwd=tmp,
                             expected=expected, source=json.dumps(value))
                return json.loads(result.stdout) if expected == 0 else result

            run([binary, '--home', home, '--command', ':help'], cwd=tmp)
            assert action({'action': 'list'})['packages'] == []
            settings = {'localDirectory': str(library), 'repository': {'kind': 'local', 'directory': str(repo)}, 'extractor': str(extractor)}
            saved = action({'action': 'configure', 'expectedRevision': action({'action':'status'})['revision'], 'settings': settings})
            assert saved['settings'] == settings
            resolved = action({'action': 'resolve', 'key': KEY, 'source': {'location': '/missing/must-not-be-read'}})
            assert resolved['from'] == 'repository' and resolved['package']['accepted']
            revision = resolved['package']['revision']
            assert hashlib.sha256(Path(resolved['descriptorPath']).read_bytes()).hexdigest() == revision
            assert not server.requests, 'library resolution invoked API or documentation'
            assert original == {p.relative_to(repo): p.read_bytes() for p in repo.rglob('*') if p.is_file()}, 'read-only repo changed'
            repo.rename(tmp / 'offline-repo')
            again = action({'action': 'resolve', 'key': KEY})
            assert again['from'] == 'local' and again['package'] == resolved['package']
            action({'action': 'resolve', 'key': KEY, 'revision': '0' * 64}, expected=1)

            endpoint = f'http://127.0.0.1:{server.server_port}/v1'
            recipe = action({'action': 'recipe', 'key': KEY, 'revision': revision,
                             'environment': 'demo', 'alias': 'inventory', 'endpoint': endpoint})
            # Use and verify the actual checked-in environment recipe, with only host-local paths/port substituted.
            example = json.loads((HERE / 'environments.yaml').read_text())
            declaration = example['environments']['demo']['imports']['inventory']
            declaration['source']['file'] = resolved['descriptorPath']
            declaration['bind']['endpoint'] = endpoint
            assert example == json.loads(recipe['recipe']), 'example recipe differs from backend pinned recipe'
            environment = tmp / 'environments.yaml'
            environment.write_text(json.dumps(example))
            result = run([binary, '--home', tmp / 'execution', '--env-file', environment, '--env', 'demo',
                          '--file', HERE / 'demo.wes'], cwd=tmp)
            reports = [json.loads(line.partition(': ')[2]) for line in result.stdout.splitlines()
                       if line.startswith('id') and ': ' in line]
            assert reports[-1] == {'name': 'Demo item', 'hasNote': False, 'removed': True}, result.stdout
            assert [r[0] for r in server.requests] == ['GET', 'PUT', 'DELETE'], server.requests
            before = len(server.requests)
            run([binary, '--home', tmp / 'execution', '--command', ':calc { return 1; }'], cwd=tmp)
            assert len(server.requests) == before, 'replay contacted API'
            print('PASS read-only repository -> local immutable revision -> actual pinned environment/script; offline reuse and inert replay')

            # Restore the repository and test a missing package with the Go extraction adapter.
            (tmp / 'offline-repo').rename(repo)
            empty = tmp / 'empty-repo'; empty.mkdir(); (empty / 'catalog.json').write_text('{"version":1,"packages":[]}')
            settings['repository'] = {'kind': 'local', 'directory': str(empty)}
            saved = action({'action': 'configure', 'expectedRevision': saved['revision'], 'settings': settings})
            new_key = {**KEY, 'scope': 'locally-generated'}
            source = {'location': str(HERE / 'repository/source.openapi.json')}
            draft = action({'action': 'resolve', 'key': new_key, 'source': source})
            assert draft['from'] == 'ingested' and not draft['package']['accepted']
            archive = library / 'sources' / (draft['package']['sourceDigest'] + '.txt')
            assert archive.read_bytes() == (HERE / 'repository/source.openapi.json').read_bytes()
            extractor.rename(tmp / 'extractor-offline')
            reused = action({'action': 'resolve', 'key': new_key, 'source': {'location': '/missing/source'}})
            assert reused['from'] == 'local' and reused['package'] == draft['package']
            action({'action': 'recipe', 'key': new_key, 'revision': draft['package']['revision'],
                    'environment': 'demo', 'alias': 'inventory', 'endpoint': endpoint}, expected=1)
            action({'action': 'accept', 'key': new_key, 'revision': draft['package']['revision']})
            (tmp / 'extractor-offline').rename(extractor)
            changed_source = tmp / 'changed.openapi.json'
            changed = json.loads((HERE / 'repository/source.openapi.json').read_text())
            changed['info']['description'] = 'Second source revision in a synthetic fixture.'
            changed_source.write_text(json.dumps(changed))
            newer = action({'action': 'ingest', 'key': new_key, 'source': {'location': str(changed_source)}})
            assert newer['package']['revision'] != draft['package']['revision']
            compared = action({'action': 'compare', 'key': new_key, 'before': draft['package']['revision'], 'after': newer['package']['revision']})
            assert compared['changes']
            print('PASS repository miss -> real Go ingestion -> archived source/draft; no repeated extraction; acceptance and explicit new revision')

            # A changed hash-addressed file must fail both resolver and actual environment admission.
            pinned = Path(resolved['descriptorPath'])
            prior = pinned.read_bytes()
            corrupt = json.loads(prior); corrupt['operations'][0]['summary'] = 'Changed outside library'
            pinned.write_text(json.dumps(corrupt))
            action({'action': 'resolve', 'key': KEY}, expected=1)
            result = run([binary, '--home', tmp / 'corrupted', '--env-file', environment, '--env', 'demo',
                          '--file', HERE / 'demo.wes'], cwd=tmp, expected=1)
            assert 'sha256' in result.stderr and len(server.requests) == before, result.stdout + result.stderr
            pinned.write_bytes(prior)
            (empty / 'catalog.json').write_text('broken')
            result = action({'action': 'resolve', 'key': {**KEY, 'scope': 'new'}, 'source': source}, expected=1)
            assert 'metadata' in result.stderr and len(server.requests) == before
            print('PASS corruption blocks resolution/import before HTTP; repository errors do not fall through to ingestion')
    finally:
        server.shutdown(); server.server_close(); thread.join(timeout=5)


if __name__ == '__main__':
    main()
