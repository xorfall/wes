#!/usr/bin/env python3
"""Acceptance of the actual project; --benchmark also measures local refresh round trips."""
import argparse
from contextlib import ExitStack
import json
import urllib.request
from pathlib import Path
import statistics
import tempfile
import time
from fixture import DemoServer, sample
from support import Client, ROOT, copy_project


def check_phase(client, last, expected_state):
    health = client.value('health', lambda d: d['last'] == last)['data']
    rows = [sample(n) for n in range(last - 19, last + 1)]
    assert health['count'] == 20 and health['state'] == expected_state, health
    assert health['errors'] == sum(r['status'] >= 500 for r in rows), health
    assert health['maxMs'] == max(r['durationMs'] for r in rows), health
    assert health['meanMs'] == sum(r['durationMs'] for r in rows) / 20, health
    assert abs(health['errorPercent'] - health['errors'] * 5) < .001, health
    table = client.value('request_table', lambda d: len(d['rows']) == 20 and d['rows'][-1][0] == str(last))['data']
    calculated = client.value('calculated_table', lambda d: len(d['rows']) == 20 and d['rows'][-1][0] == str(last))['data']
    assert table == calculated
    assert table['rows'] == [[str(r['seq']), r['route'], str(r['durationMs']), str(r['status'])] for r in rows]
    line = client.value('latency_timeline', lambda d: len(d['series'][0]['samples']) == 20 and d['series'][0]['samples'][-1]['id'] == str(last))['data']
    assert [p['value'] for p in line['series'][0]['samples']] == [r['durationMs'] for r in rows], line
    metric = client.value('health_metric', lambda d: d['view'] == 'metric' and d['status'] == expected_state and abs(float(d['value']) - float(health['meanMs'])) < .001)['data']
    assert abs(float(metric['value']) - float(health['meanMs'])) < .001 and metric['unit'] == 'ms', metric
    histogram = client.value('latency_histogram', lambda d: d['total'] == len(rows))['data']
    assert histogram['total'] == 20 and sum(b['count'] for b in histogram['bins']) == 20
    client.value('health_display', lambda d: d.startswith(expected_state))
    return health


def benchmark(client):
    # Input is frozen. Equal visible table content; the typed definition validates its input/output contracts.
    client.submit(':workspace policy mode:manual')
    times = {'inline': [], 'typed': []}
    names = {'inline': 'calculated_table', 'typed': 'request_table'}
    for iteration in range(35):
        for kind in (('inline', 'typed') if iteration % 2 else ('typed', 'inline')):
            after = client.serial
            start = time.perf_counter()
            client.submit(':refresh $' + names[kind])
            client.value(names[kind], after=after)
            if iteration >= 5:
                times[kind].append((time.perf_counter() - start) * 1000)
    return {'scope': '20 rows, release/debug as supplied; submit -> published value read, including HTTP and storage; 5 warmup + 30 paired samples; no GUI draw/network source',
            'milliseconds': {k: {'median': round(statistics.median(v), 3), 'p95': round(sorted(v)[int(.95 * (len(v)-1))], 3)} for k, v in times.items()}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/wes')
    parser.add_argument('--benchmark', action='store_true')
    args = parser.parse_args()
    server = DemoServer()
    try:
        with ExitStack() as stack:
            folder = stack.enter_context(tempfile.TemporaryDirectory(prefix='wes-monitor-check-'))
            root = Path(folder)
            copy_project(root, server.server_port)
            client = Client(args.binary.resolve(), root)
            stack.callback(client.close)
            client.prepare()
            empty = client.value('health')['data']
            assert empty['state'] == 'WAITING' and empty['count'] == 0, empty
            assert client.value('latency_histogram')['data']['total'] == 0
            for last, state in [(20, 'HEALTHY'), (40, 'DEGRADED'), (60, 'HEALTHY')]:
                server.publish([sample(n) for n in range(last - 19, last + 1)])
                check_phase(client, last, state)
            # V2 full constraints reject a structurally valid negative latency as well as bad JSON.
            bad = sample(61)
            bad['durationMs'] = -1
            server.publish([bad, '{malformed', sample(61)])
            client.value('health', lambda d: d['last'] == 61)
            client.wait(lambda: 'Stream rejected 2 invalid items.' in client.ready[client.names['requests']]['cautions'])
            server.publish([sample(n) for n in range(62, 621)])
            check_phase(client, 620, 'HEALTHY')
            window = client.value('requests', lambda d: len(d) == 500 and d[-1]['seq'] == 620)['data']
            assert window[0]['seq'] == 121
            assert len(server.requests) == 1 and server.active == 1, server.requests
            # Refreshing either local representation cannot reconnect the producer.
            for name in ['request_table', 'calculated_table']:
                after = client.serial
                client.submit(':refresh $' + name)
                client.value(name, after=after)
            if args.benchmark:
                print(json.dumps(benchmark(client), indent=2))
            assert len(server.requests) == 1
            # No Keep: cancellation preserves display observations, not live graph inputs.
            observed = ['requests', 'recent', 'health', 'health_display', 'health_metric', 'request_table', 'latency_timeline', 'durations', 'latency_histogram', 'calculated_table']
            last_values = {name: client.value(name)['data'] for name in observed}
            client.submit(':cancel $requests')
            with server.changed:
                assert server.changed.wait_for(lambda: server.active == 0, 10)
            for name in observed:
                client.wait(lambda: client.ready.get(client.names[name], {}).get('event') == 'stopped')
                assert client.value(name)['data'] == last_values[name], name
                frame = client.ready[client.names[name]]
                assert frame['state'] in ('cancelled', 'skipped') and not frame['kept'], frame
                assert frame['source'] == client.names['requests'] and frame['run'], frame
            # A new UI connection reconstructs the stopped observations, not just a cached screen.
            with urllib.request.urlopen(client.url + '/events', timeout=10) as reconnect:
                recovered = {}
                for raw in reconnect:
                    if raw.startswith(b'data:'):
                        event = json.loads(raw[5:])
                        if event['event'] == 'stopped':
                            recovered[event['node']] = event
                            if all(client.names[name] in recovered for name in observed):
                                break
                assert all(recovered[client.names[name]]['handle'] == client.ready[client.names[name]]['handle'] for name in observed)
            after = client.serial
            client.submit(':refresh $requests')
            client.value('requests', lambda d: len(d) == 500 and d[-1]['seq'] == 620, after=after)
            client.wait(lambda: all(client.ready.get(client.names[name], {}).get('event') == 'ready' for name in observed))
            check_phase(client, 620, 'HEALTHY')
            assert len(server.requests) == 2, server.requests
            # Automatic retention is disabled in this isolated demo: keep finite results explicitly.
            for name in list(client.names):
                client.keep(name)
            client.submit(':workspace save "monitor-snapshot"')
            before = client.generation
            client.submit(':workspace load "monitor-snapshot"', switch=True)
            client.wait(lambda: client.generation != before)
            client.value('health', lambda d: d['last'] == 620)
            assert server.active == 1, 'selecting another workspace stopped accepted work'
            assert len(server.requests) == 2, 'held load reconnected the source'
            # Returning to the live name reuses its original subscription and generation.
            client.submit(':workspace load "default"', switch=True)
            client.wait(lambda: client.generation == before)
            client.submit(':cancel $requests')
            with server.changed:
                assert server.changed.wait_for(lambda: server.active == 0, 10), 'explicit cancel did not close the subscription'
            assert len(server.requests) == 2, 'returning to the original workspace reconnected the source'
            print('PASS actual monitor files: empty/healthy/degraded/recovered, exact table/timeline/histogram, contract rejection, 500-item rolling window, inline/typed calc equivalence, single subscription and inert saved restore')
    finally:
        server.close()


if __name__ == '__main__':
    main()
