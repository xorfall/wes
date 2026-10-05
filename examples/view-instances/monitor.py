#!/usr/bin/env python3
"""Run an isolated, synthetic HTTP/SSE Timeline dashboard.

Prerequisites: cargo build -p wes; npm --prefix gui run build.
Run: python3 examples/view-instances/monitor.py --binary target/debug/wes
Open the printed URL, open $dashboard, then Apply. Stop query closes its SSE request.
Range selection and picked events stay local to this workspace. No credentials needed.
The source keeps at most 1,000 test events; the engine keeps its bounded stream window.
--check executes these actual .wes definitions and checks provider ownership and Stop.
"""
import argparse
from contextlib import ExitStack
import json
from pathlib import Path
import sys
import tempfile
import time
import urllib.request
import urllib.error

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "examples/live-service-monitor"))
from fixture import DemoServer, sample
from support import Client, copy_project


def setup(client, members=1):
    client.submit(':env plan file:environments.yaml > plan')
    client.submit(':env apply $plan')
    client.wait(lambda: client.context)
    client.submit(':env use "monitor-demo"')
    client.submit(':package load path:types.yaml')
    client.file('monitor-layout.wes')
    client.value('layout')
    client.value('dashboardLayout')
    client.file('monitor.wes')
    client.submit(':view create TimelineGroup input:$layout > monitor')
    client.value('monitor')
    client.submit(':calc { return {view:"choice",title:"Feed",options:[{value:"demo",label:"Synthetic requests"}]}; } > feedData')
    client.value('feedData')
    client.submit(':view create Choice input:$feedData > feed')
    client.value('feed')
    for i in range(members):
        name = 'requests' if i == 0 else f'requests{i+1}'
        client.submit(f':view create Timeline > {name}')
        client.value(name)
        client.submit(f':view connect ${name} to:$monitor > connection{i}')
        client.value(f'connection{i}')
        client.submit(f':view query ${name} template:WatchRequests from:$feed output:value mode:live adapter:RequestTimeline > query{i}')
        client.value(f'query{i}')
    client.submit(':view create Dashboard input:$dashboardLayout > dashboard')
    client.value('dashboard')
    client.submit(':view connect $feed to:$dashboard > feedConnection')
    client.value('feedConnection')
    client.submit(':view connect $monitor to:$dashboard > groupConnection')
    client.value('groupConnection')
    return client.value('dashboard')['data']


def frame(client, view):
    request = urllib.request.Request(client.url + f'/view-instances/{view["id"]}/{view["instance"]}',
                                     headers={'X-Wes-Session': client.generation})
    with urllib.request.urlopen(request, timeout=5) as response:
        raw = response.read()
    return json.loads(raw), len(raw)


def check(client, server, view, members=1):
    assert not server.requests, 'Opening and binding must not call the provider'
    feed = client.value('feed')['data']
    url = client.url + f'/view-interaction/{feed["id"]}/{feed["instance"]}'
    headers = {'X-Wes-Session': client.generation, 'Content-Type': 'application/json'}
    with urllib.request.urlopen(urllib.request.Request(url, headers=headers)) as response:
        state = json.load(response)
    state = {key: state[key] for key in ('owner', 'identity', 'definitionRevision', 'revision')}
    state.update(fields={'value':'demo'}, outputs={'value':'demo'})
    with urllib.request.urlopen(urllib.request.Request(url, json.dumps(state).encode(), headers, method='PUT')) as response:
        assert response.status == 200
    for i in range(members):
        # A different window/client keeps the binding's target, not its own default environment.
        client.post({'request':'submit','client':'independent-window','cell':f'apply-{i}',
                     'text':':view apply $' + ('requests' if i == 0 else f'requests{i+1}'),
                     'environments':{'selected':None,'revisions':client.context['revisions']}})
    with server.changed:
        assert server.changed.wait_for(lambda: server.active == members, 5), 'Apply did not open the stream'
    server.publish([sample(i) for i in range(1, 601)])
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        try:
            result, size = frame(client, view)
        except urllib.error.HTTPError as error:
            if error.code not in (409, 429, 503):
                raise
            time.sleep(.1)
            continue
        entries = [e for e in result['instances'] if e['definition'] == 'timeline']
        entry = entries[-1]
        if entry.get('inputProblem'):
            raise AssertionError(entry['inputProblem'])
        if entry.get('input') and entry['input']['data'].get('series'):
            rows = entry['input']['data']['series'][0]['samples']
            if rows and rows[-1]['id'] == '600' and all(e.get('input') and e['input']['data'].get('series') and e['input']['data']['series'][0]['samples'] and e['input']['data']['series'][0]['samples'][-1]['id'] == '600' for e in entries):
                assert len(rows) <= 120
                assert entry['inputCautions'], 'Window omission must remain visible'
                assert size < 1024 * 1024
                break
        time.sleep(.1)
    else:
        raise AssertionError(f'No bounded Timeline output: {result}')
    started = time.monotonic()
    client.submit(':view stop $dashboard')
    accepted = time.monotonic() - started
    with server.changed:
        assert server.changed.wait_for(lambda: server.active == 0, 5), 'Stop did not close SSE'
    print(json.dumps({'check': 'passed', 'retained_samples': len(rows), 'frame_bytes': size,
                      'stop_accepted_ms': round(accepted * 1000, 2), 'provider_requests': len(server.requests)}))
    before = len(server.requests)
    document = (client.root/'environments.yaml').read_text().replace(f':{server.server_port}', f':{server.server_port}/changed')
    (client.root/'environments.yaml').write_text(document)
    client.submit(':env plan file:environments.yaml > changedPlan')
    client.submit(':env apply $changedPlan')
    client.post({'request':'submit','client':'independent-window','cell':'stale-apply',
                 'text':':view apply $requests','environments':client.context})
    deadline = time.monotonic()+5
    while not client.problem and time.monotonic()<deadline:
        time.sleep(.02)
    assert client.problem and 'Query environment changed' in client.problem, client.problem
    assert len(server.requests) == before, 'Changed environment must fail before provider entry'
    print('PASS changed environment requires explicit query rebinding')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT/'target/debug/wes')
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--members', type=int, choices=(1,4,8), default=1)
    parser.add_argument('--interval', type=float, default=.05)
    args = parser.parse_args()
    if args.interval <= 0:
        parser.error('--interval must be positive')
    server = DemoServer()
    try:
        with ExitStack() as stack:
            root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='wes-view-monitor-')))
            copy_project(root, server.server_port)
            (root/'monitor.wes').write_text((HERE/'monitor.wes').read_text())
            (root/'monitor-layout.wes').write_text((HERE/'monitor-layout.wes').read_text())
            client = Client(args.binary.resolve(), root, site=not args.check)
            stack.callback(client.close)
            view = setup(client, args.members)
            if args.check:
                check(client, server, view, args.members)
                return
            print(f'{client.url}\nOpen $dashboard and press Apply. Ctrl-C stops this isolated demo.', flush=True)
            seq = 1
            while client.process.poll() is None:
                if client.problem:
                    raise RuntimeError(client.problem)
                server.publish([sample(seq)])
                seq += 1
                if seq >= 60000:
                    server.finish()
                    break
                time.sleep(args.interval)
    except KeyboardInterrupt:
        pass
    finally:
        server.close()


if __name__ == '__main__':
    main()
