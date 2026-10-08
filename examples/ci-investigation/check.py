#!/usr/bin/env python3
"""Exercise independently synthetic execution evidence through native Wes scans.

No provider is invoked and no credentials are read. The harness submits whole
artifacts; framing, transition state, checkpoints and output paging belong to Wes.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import selectors
import subprocess
import tempfile
import threading
import time
import urllib.request
import uuid

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
DEFAULT_BINARY = ROOT / 'target/debug/wes'


def literal(value):
    return json.dumps(value, ensure_ascii=False)


def untag(value):
    if isinstance(value, dict):
        if value == {'kind': 'none'}:
            return None
        if set(value) == {'kind', 'value'} and value['kind'] == 'some':
            return untag(value['value'])
        return {key: untag(item) for key, item in value.items()}
    if isinstance(value, list):
        return [untag(item) for item in value]
    return value


def value_command(name, value, contract=None):
    schema = ', ' + literal(contract) if contract else ''
    return ':calc pure { return parseJson(' + literal(literal(value)) + schema + '); } > ' + name


class Application:
    def __init__(self, binary, root, large=False):
        user = root / 'user'
        user.mkdir(mode=0o700, exist_ok=True)
        if large:
            settings = user / '.wes-settings'
            settings.mkdir(mode=0o700)
            (settings / 'limits.json').write_text(literal({'version': 1, 'revision': 0, 'values': {'scan.work': 300_000_000}}))
        environment = {key: value for key, value in os.environ.items()
                       if not key.startswith(('WES_', 'GH_', 'GITHUB_'))
                       and not any(word in key.upper() for word in ('TOKEN', 'SECRET', 'PASSWORD', 'API_KEY'))}
        environment.update(HOME=str(user), USERPROFILE=str(user), PATH='/usr/bin:/bin')
        self.log = (root / 'application.log').open('a')
        self.process = subprocess.Popen([str(binary), '--home', str(root / 'data'),
                                         '--serve', '0', '--no-auto-keep'],
                                        cwd=root, env=environment, stdout=subprocess.PIPE,
                                        stderr=self.log, text=True)
        with selectors.DefaultSelector() as selector:
            selector.register(self.process.stdout, selectors.EVENT_READ)
            assert selector.select(30), 'application startup timed out'
        self.url = self.process.stdout.readline().strip().removeprefix('Listening at ')
        assert self.url.startswith('http://127.0.0.1:'), self.url
        self.stream = urllib.request.urlopen(self.url + '/events', timeout=120)
        self.events = []
        self.incoming = queue.Queue()
        self.reader = threading.Thread(target=self.read_events, daemon=True)
        self.reader.start()
        self.generation = self.until(lambda event: event.get('event') == 'session')['generation']

    def read_events(self):
        try:
            for line in self.stream:
                if line.startswith(b'data:'):
                    self.incoming.put(json.loads(line[5:]))
        except Exception as error:
            self.incoming.put(error)
        finally:
            self.incoming.put(EOFError('application event stream closed'))

    def until(self, predicate, after=0):
        for event in self.events[after:]:
            if predicate(event):
                return event
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            event = self.incoming.get(timeout=max(0.01, deadline - time.monotonic()))
            if isinstance(event, Exception):
                raise event
            self.events.append(event)
            if predicate(event):
                return event
        raise AssertionError({'event_timeout': self.events[-5:]})

    def post(self, path, body):
        request = urllib.request.Request(self.url + path, literal(body).encode(),
                                         {'Content-Type': 'application/json', 'X-Wes-Session': self.generation})
        with urllib.request.urlopen(request, timeout=30) as response:
            result = response.read()
            return json.loads(result) if result else None

    def submit(self, source):
        cell = str(uuid.uuid4())
        after = len(self.events)
        self.post('/submit', {'request': 'submit', 'client': 'synthetic-investigation', 'cell': cell, 'text': source})
        report = self.until(lambda event: event.get('event') == 'reported' and event.get('cell') == cell, after)
        errors = [item for item in report.get('diagnostics', []) if item.get('severity') == 'error']
        assert not errors, errors
        return after

    def ready(self, name, after=0):
        node = self.until(lambda event: event.get('event') == 'created' and event.get('name') == name, after)['node']
        event = self.until(lambda event: event.get('node') == node and event.get('event') in
                           ('ready', 'evidence', 'failed', 'cancelled'), after)
        assert event['event'] == 'ready', {'event': event, 'receipt': self.read(event).get('receipt') if event.get('handle') else None}
        return event

    def read(self, event):
        with urllib.request.urlopen(self.url + '/values/' + event['handle'], timeout=30) as response:
            return untag(json.load(response)['data'])

    def view(self, definition, input_name, name, expected):
        after = self.submit(':view create ' + definition + ' input:$' + input_name + ' > ' + name)
        event = self.ready(name, after)
        description = self.read(event)
        request = urllib.request.Request(self.url + '/view-instances/' + event['node'] + '/' + description['instance'],
                                         headers={'X-Wes-Session': self.generation})
        with urllib.request.urlopen(request, timeout=30) as response:
            frame = json.load(response)
        instance = frame['instances'][0]
        assert instance['inputProblem'] is None, instance
        assert untag(instance['input']['data']) == expected
        assert instance['definition'] == {'FailureSummary': 'failure-summary', 'LogExcerpt': 'log-excerpt',
                                          'SpanTimeline': 'span-timeline', 'ExecutionComparison': 'execution-comparison'}[definition]

    def rows(self, event, stream='outputs'):
        result = []
        position = '0'
        while True:
            request = urllib.request.Request(self.url + '/datasets/' + event['handle'] +
                                             '?select=/outputs&stream=' + stream + '&from=' + position + '&limit=100',
                                             headers={'X-Wes-Session': self.generation})
            with urllib.request.urlopen(request, timeout=30) as response:
                page = json.load(response)['page']
            assert page['first'] == position
            result.extend(untag(row['value']['data']) for row in page['rows'])
            if page['extentExhausted']:
                return result
            assert int(page['next']) > int(position), page
            position = page['next']

    def close(self):
        self.process.terminate()
        try:
            self.process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
        self.stream.close()
        self.reader.join(timeout=5)
        self.log.close()


SEED = {'lines': 0, 'lastOrdinal': 0, 'outsideScope': 0, 'recognized': 0, 'outer': None, 'tapTitle': '', 'pending': '',
        'pendingFrom': 0, 'pendingByte': 0, 'lastByte': 0, 'finished': False}


def scan(app, name, lines, provider='github', envelope='tabbed', constraint=None, envelope_prefixes=None):
    prefixes = envelope_prefixes or ['Worker\tTests\t'] * len(lines)
    assert len(prefixes) == len(lines)
    raw = '\n'.join((prefix + line) if envelope == 'tabbed' else line for prefix, line in zip(prefixes, lines)) + ('\n' if lines else '')
    artifact = {'run': name, 'attempt': 1, 'job': 'worker', 'origin': 'synthetic:' + name,
                'digest': hashlib.sha256(raw.encode()).hexdigest()}
    context = {'provider': provider, 'run': name, 'attempt': 1, 'artifact': artifact,
               'envelope': envelope, 'jobName': 'Worker', 'stepName': 'Tests', 'constraint': constraint}
    source = '\n'.join([value_command(name + 'Raw', raw), value_command(name + 'Seed', SEED, 'InvestigationState'),
                         value_command(name + 'Context', context, 'InvestigationContext'),
                         ':scan source:$' + name + 'Raw transition:InvestigateRecord finish:FinishInvestigation '
                         'initial:$' + name + 'Seed context:$' + name + 'Context profile:LinesUtf8 sink:dataset > ' + name])
    started = time.monotonic()
    after = app.submit(source)
    event = app.ready(name, after)
    value = app.read(event)
    rows = app.rows(event)
    assert value['receipt']['status'] == 'complete', value['receipt']
    assert value['receipt']['inputRecords'] == len(lines)
    assert value['state']['lines'] == len(lines) and value['state']['finished']
    assert value['state']['recognized'] == len(rows)
    assert len({row['id'] for row in rows}) == len(rows)
    for row in rows:
        assert row['artifact'] == artifact and 1 <= row['from'] <= row['to'] <= len(lines)
        original = raw.encode()[row['byteStart']:row['byteEnd']].decode()
        assert original.endswith('\n'), row
        selected = '\n'.join(raw.splitlines()[row['from'] - 1:row['to']]) + '\n'
        assert original == selected, row
        excerpt_name = name + 'Excerpt' + str(row['from'])
        offset = literal(str(row['byteStart']))
        size = row['byteEnd'] - row['byteStart']
        if size <= 65536:
            since = app.submit(':scan excerpt $' + name + ' from:' + offset + ' limit:' + str(size) + ' > ' + excerpt_name)
            excerpt = app.read(app.ready(excerpt_name, since))
            assert excerpt['data'] == original and excerpt['unit'] == 'bytes'
            evidence_context = {'artifact': artifact, 'first': row['from'], 'envelope': envelope,
                                'jobName': context['jobName'], 'stepName': context['stepName'],
                                'title': row['title'], 'focusFrom': row['from'], 'focusTo': row['to']}
            since = app.submit(value_command(excerpt_name + 'Context', evidence_context, 'InvestigationEvidenceContext') + '\n' +
                               ':scan source:$' + excerpt_name + '.data transition:ReadEvidenceLine initial:0 '
                               'context:$' + excerpt_name + 'Context profile:LinesUtf8 sink:memory > ' + excerpt_name + 'Lines')
            app.ready(excerpt_name + 'Lines', since)
            since = app.submit('EvidenceView result:$' + excerpt_name + 'Lines context:$' + excerpt_name + 'Context > ' + excerpt_name + 'View')
            view = app.read(app.ready(excerpt_name + 'View', since))
            assert view['view'] == 'log-excerpt' and view['artifact'] == artifact
            assert [line['ordinal'] for line in view['lines']] == list(range(row['from'], row['to'] + 1))
            assert view['focus'] == {'from': row['from'], 'to': row['to']}
            if name == 'nested' and row['format'] == 'node-assertion':
                app.view('LogExcerpt', excerpt_name + 'View', excerpt_name + 'Rendered', view)
    after = app.submit(value_command(name + 'Rows', rows, 'List<InvestigationObservation>') + '\n' +
                       'InvestigationSummary observations:$' + name + 'Rows state:$' + name + '.state receipt:$' + name + '.receipt context:$' + name + 'Context > ' + name + 'Summary')
    summary = app.read(app.ready(name + 'Summary', after))
    assert len(summary['failures']) == len(rows) and summary['originating'] == []
    if name == 'nested':
        app.view('FailureSummary', name + 'Summary', name + 'SummaryView', summary)
    if lines:
        assert summary['coverage']['read'][0]['complete']
    else:
        assert summary['coverage']['read'] == []
    print(f'PASS {name}: {len(lines)} native frames, {len(rows)} compact observations, '
          f'{value["receipt"]["work"]} work, {time.monotonic() - started:.2f}s', flush=True)
    return rows, summary, value


def acceptance(app, large=False):
    packages = [ROOT / 'views' / name / 'types.yaml' for name in ('failure-summary', 'log-excerpt', 'span-timeline')]
    packages.append(HERE / 'investigation-types.yaml')
    app.submit('\n'.join(':package load path:' + literal(str(path)) for path in packages) + '\n' + (HERE / 'investigate.wes').read_text())
    noise = ['test handles_error_without_panicking ... ok'] * 126
    nested = ["thread 'paced_reader' (8) panicked at src/runner.rs:24:",
              'not ok 1 - responsive input',
              'AssertionError [ERR_ASSERTION]: p95 412.0 ms is not < 180.0 ms; baseline 960.0 ms',
              'test result: FAILED. 0 passed; 1 failed',
              'Traceback (most recent call last):', '  File "run.py", line 17, in main',
              'subprocess.CalledProcessError: nested test exited with status 101',
              '##[error]Process completed with exit code 1.']
    rows, summary, value = scan(app, 'nested', noise + nested + ['completed capture'] * (3900 if large else 512))
    assert [row['format'] for row in rows] == ['rust-panic', 'node-assertion', 'python-traceback', 'provider-report']
    node = rows[1]
    assert node['parent'] == rows[0]['id'] and node['measurement']['comparator'] == '<'
    assert str(node['measurement']['value']) == '412.0' and str(node['measurement']['baseline']) == '960.0'
    assert node['measurement']['metric'] == 'p95' and len(summary['edges']) == 1
    assert value['receipt']['outputRecords'] == 4, 'compact mode must not retain every normalized row'
    assert value['receipt']['limits']['work'] == (300_000_000 if large else 64_000_000)
    assert value['receipt']['limits']['outputCharge'] == 16_777_216
    rows, _, _ = scan(app, 'boundary', ['ordinary output'] * 127 +
                      ['Traceback (most recent call last):', '  File "batch.py", line 8, in execute',
                       '    submit()', 'ValueError: invalid batch input'])
    assert len(rows) == 1 and (rows[0]['from'], rows[0]['to']) == (128, 131)
    rows, _, _ = scan(app, 'invocation', ['usage: validate --input FILE', 'validate: error: the following arguments are required: --input'])
    assert rows[0]['kind'] == 'invocation' and rows[0]['from'] == 1 and rows[0]['to'] == 2
    rows, _, _ = scan(app, 'dependency', ['npm warn deprecated old-package', 'npm error code ERESOLVE'])
    assert len(rows) == 1 and rows[0]['kind'] == 'dependency'
    rows, _, _ = scan(app, 'passing', ['test error_is_a_valid_field ... ok', 'npm warn network retried', 'unknown-format: ERROR_LABEL=ordinary-data'])
    assert rows == [], 'unclassified text must not be diagnosed as a failure'
    assert scan(app, 'empty', [])[0] == []
    assert scan(app, 'unicode', ['snow 雪', ''])[0] == []

    # An independently synthetic oversized log record is an explicit skipped input,
    # not an interpreted diagnostic. Its summary must retain the original ordinal
    # range and must never claim the skipped record was interpreted.
    raw = 'ordinary output\n' + 'x' * 65537 + '\nnpm error code ERESOLVE\n'
    artifact = {'run': 'malformed', 'attempt': 1, 'job': 'worker', 'origin': 'synthetic:malformed',
                'digest': hashlib.sha256(raw.encode()).hexdigest()}
    context = {'provider': 'batch-scheduler', 'run': 'malformed', 'attempt': 1, 'artifact': artifact,
               'envelope': 'plain', 'jobName': 'Worker', 'stepName': 'Tests', 'constraint': None}
    after = app.submit(value_command('malformedRaw', raw) + '\n' +
                       value_command('malformedSeed', SEED, 'InvestigationState') + '\n' +
                       value_command('malformedContext', context, 'InvestigationContext') + '\n' +
                       ':scan source:$malformedRaw transition:InvestigateRecord finish:FinishInvestigation '
                       'initial:$malformedSeed context:$malformedContext profile:LinesUtf8 sink:dataset '
                       'malformed:forensic excerpt:32 > malformed')
    event = app.ready('malformed', after)
    result = app.read(event)
    receipt = result['receipt']
    assert receipt['status'] == 'complete' and receipt['malformed'] == 'forensic', receipt
    assert (receipt['inputRecords'], receipt['rejectedRecords'], receipt['rejectedInputBytes']) == (3, 1, 65538), receipt
    assert result['state']['lines'] == 2 and result['state']['finished']
    assert result['outputs']['reference']['records'] == '1'
    observations = app.rows(event)
    assert len(observations) == 1 and observations[0]['from'] == 3 and observations[0]['kind'] == 'dependency'
    rejected = app.rows(event, 'coverage')
    assert len(rejected) == 1 and rejected[0]['recordOrdinal'] == '1' and rejected[0]['reason'] == 'raw_limit'
    assert rejected[0]['sourceStart'] == '16' and rejected[0]['delimiterEnd'] == '65554'
    import base64
    assert base64.b64decode(rejected[0]['excerpt'], validate=True) == b'x' * 32
    assert rejected[0]['excerptTruncated'] is True
    after = app.submit(':dataset page $malformed.outputs stream:coverage > malformedPage\n'
                       ':scan excerpt $malformed from:16 limit:32 > malformedOriginal')
    page = app.read(app.ready('malformedPage', after))
    assert page['stream'] == 'coverage' and page['records'] == '1' and page['rows'] == rejected
    assert app.read(app.ready('malformedOriginal', after))['data'] == 'x' * 32
    after = app.submit(value_command('malformedRows', observations, 'List<InvestigationObservation>') + '\n' +
                       'InvestigationSummary observations:$malformedRows state:$malformed.state receipt:$malformed.receipt context:$malformedContext > malformedSummary')
    summary = app.read(app.ready('malformedSummary', after))
    assert summary['coverage']['read'][0]['to'] == 3 and not summary['coverage']['read'][0]['complete']
    assert summary['coverage']['notRead'][0]['reason'].startswith('1 framed records rejected')
    oversized = 'x' * 65537 + '\n'
    for name, raw, expected in [
        ('rejectedBeforePending', oversized + 'Traceback (most recent call last):\n', (2, 2)),
        ('rejectedInsidePending', 'Traceback (most recent call last):\n' + oversized + 'ValueError: invalid\n', (1, 1)),
        ('rejectedAfterPending', 'Traceback (most recent call last):\n' + oversized, (1, 1)),
    ]:
        case_context = {**context, 'run': name, 'artifact': {**artifact, 'run': name, 'digest': hashlib.sha256(raw.encode()).hexdigest()}}
        after = app.submit(value_command(name + 'Raw', raw) + '\n' +
                           value_command(name + 'Seed', SEED, 'InvestigationState') + '\n' +
                           value_command(name + 'Context', case_context, 'InvestigationContext') + '\n' +
                           ':scan source:$' + name + 'Raw transition:InvestigateRecord finish:FinishInvestigation '
                           'initial:$' + name + 'Seed context:$' + name + 'Context profile:LinesUtf8 sink:dataset '
                           'malformed:forensic excerpt:32 > ' + name)
        event = app.ready(name, after)
        result = app.read(event)
        assert result['receipt']['status'] == 'complete' and result['receipt']['rejectedRecords'] == 1
        rows = app.rows(event)
        assert len(rows) == 1 and (rows[0]['from'], rows[0]['to']) == expected, rows
        assert rows[0]['confidence'] == 'uncertain', rows
        assert raw.encode()[rows[0]['byteStart']:rows[0]['byteEnd']] == b'Traceback (most recent call last):\n'
        after = app.submit(value_command(name + 'Rows', rows, 'List<InvestigationObservation>') + '\n' +
                           'InvestigationSummary observations:$' + name + 'Rows state:$' + name +
                           '.state receipt:$' + name + '.receipt context:$' + name + 'Context > ' + name + 'Summary')
        summary = app.read(app.ready(name + 'Summary', after))
        assert summary['coverage']['read'][0]['to'] == result['receipt']['inputRecords']
        assert not summary['coverage']['read'][0]['complete'] and len(summary['coverage']['notRead']) == 1
    print('PASS forensic CI framing: exact original ordinals, incomplete coverage and no diagnostic linkage across rejected bytes', flush=True)
    rows, _, _ = scan(app, 'truncated', ['Traceback (most recent call last):', '  File "partial.py", line 2'])
    assert rows[0]['confidence'] == 'uncertain' and rows[0]['title'].startswith('Incomplete')
    rows, _, _ = scan(app, 'interruptedBlock', ['Traceback (most recent call last):', '  File "partial.py", line 2',
                                               'usage: submit --job NAME', 'submit: error: missing --job'])
    assert [(row['from'], row['to'], row['confidence']) for row in rows] == [(1, 2, 'uncertain'), (3, 4, 'observed')]
    rows, summary, result = scan(app, 'scopedJobs', ['Traceback (most recent call last):',
                                                   'AssertionError [ERR_ASSERTION]: unrelated job',
                                                   'AssertionError [ERR_ASSERTION]: selected job'],
                                 envelope_prefixes=['Worker\tTests\t', 'Other\tTests\t', 'Worker\tTests\t'])
    assert [(row['from'], row['to'], row['confidence']) for row in rows] == [(1, 1, 'uncertain'), (3, 3, 'observed')]
    assert all(row['parent'] is None for row in rows) and summary['edges'] == []
    assert result['state']['outsideScope'] == 1 and '1 records outside' in summary['coverage']['read'][0]['scope']
    rows, _, _ = scan(app, 'batch', ['TASK prepare outcome=passed', 'TASK transform outcome=failed',
                                     'TASK publish outcome=cancelled'], provider='batch-scheduler', envelope='plain')
    assert [row['format'] for row in rows] == ['task-outcome', 'task-outcome']
    assert all(row['kind'] == 'unknown' for row in rows), 'a task outcome does not reveal its cause'
    samples = literal({'before': {'transport': 'legacy', 'keys': 42, 'median_ms': 86.25, 'p95_ms': 760.5, 'max_ms': 803.25},
                       'after': {'transport': 'new', 'keys': 42, 'median_ms': 92.5, 'p95_ms': 321.25, 'max_ms': 402.5}}).replace(' ', '')
    rows, _, _ = scan(app, 'metricsUnknown', [samples], envelope='plain')
    assert str(rows[0]['measurement']['value']) == '321.25' and rows[0]['measurement']['comparator'] is None
    rows, _, _ = scan(app, 'metricsConstraint', [samples], envelope='plain',
                      constraint={'metric': 'p95', 'unit': 'ms', 'comparator': '<', 'threshold': 175.0, 'source': 'synthetic captured assertion v1'})
    assert rows[0]['measurement']['comparator'] == '<' and str(rows[0]['measurement']['threshold']) == '175.0'
    assert rows[0]['measurement']['constraintSource'] == 'synthetic captured assertion v1'

    digest = hashlib.sha256(b'independently synthetic batch input v1').hexdigest()
    environment = hashlib.sha256(b'synthetic environment v1').hexdigest()
    subject = {'provider': 'batch-scheduler', 'run': 'job-red', 'attempt': 1, 'inputDigest': digest,
               'target': 'transform', 'targetOutcome': 'failed', 'environmentDigest': None, 'availability': 'available'}
    cases = [('skipped', 'not_run', None, digest, 'incomparable'),
             ('unknownEnvironment', 'passed', None, digest, 'different_outcomes'),
             ('equalEnvironment', 'passed', environment, digest, 'suspected_intermittence'),
             ('changedInput', 'passed', environment, hashlib.sha256(b'different input').hexdigest(), 'incomparable')]
    compared = []
    for name, outcome, env, source_digest, expected in cases:
        current = {**subject, 'environmentDigest': env}
        baseline = {**current, 'run': 'job-green', 'targetOutcome': outcome, 'inputDigest': source_digest}
        after = app.submit(value_command(name + 'Subject', current, 'InvestigationExecution') + '\n' +
                           value_command(name + 'Baseline', baseline, 'InvestigationExecution') + '\n' +
                           'CompareExecutions subject:$' + name + 'Subject baseline:$' + name + 'Baseline > ' + name)
        result = app.read(app.ready(name, after))
        assert result['hypothesis'] == expected and not result['regressionRuledOut'], result
        assert result['targetExercised'] == (outcome != 'not_run')
        compared.append(result)
    after = app.submit(value_command('compared', compared, 'List<InvestigationComparison>') + '\n' +
                       'ExecutionComparisonInput comparisons:$compared title:"Synthetic execution comparisons" > comparisonReport')
    report = app.read(app.ready('comparisonReport', after))
    assert report['comparisons'] == compared
    app.view('ExecutionComparison', 'comparisonReport', 'comparisonView', report)
    print('PASS target-not-run, unknown environment, matching environment and non-Git input comparison', flush=True)

    artifact = {'run': 'unavailable-job', 'attempt': 2, 'job': 'worker', 'origin': 'synthetic:availability', 'digest': digest}
    context = {'provider': 'batch-scheduler', 'run': artifact['run'], 'attempt': 2, 'artifact': artifact,
               'envelope': 'plain', 'jobName': 'Worker', 'stepName': 'Tests', 'constraint': None}
    app.submit(value_command('unavailableContext', context, 'InvestigationContext'))
    for availability in ('not_yet_available', 'forbidden', 'expired', 'missing'):
        name = availability.replace('_', '')
        after = app.submit('UnavailableInvestigation context:$unavailableContext availability:' + availability + ' > ' + name)
        value = app.read(app.ready(name, after))
        assert value['failures'] == [] and value['coverage']['read'] == []
        assert availability in value['coverage']['notRead'][0]['reason']

    bad_context = {**context, 'attempt': 1}
    after = app.submit(value_command('wrongAttemptRaw', 'ordinary output\n') + '\n' +
                       value_command('wrongAttemptSeed', SEED, 'InvestigationState') + '\n' +
                       value_command('wrongAttemptContext', bad_context, 'InvestigationContext') + '\n' +
                       ':scan source:$wrongAttemptRaw transition:InvestigateRecord initial:$wrongAttemptSeed '
                       'context:$wrongAttemptContext profile:LinesUtf8 sink:dataset > wrongAttempt')
    node = app.until(lambda event: event.get('event') == 'created' and event.get('name') == 'wrongAttempt', after)['node']
    refused = app.until(lambda event: event.get('node') == node and event.get('event') in ('ready', 'evidence', 'failed'), after)
    assert refused['event'] != 'ready', refused
    if refused.get('handle'):
        receipt = app.read(refused)['receipt']
        assert receipt['status'] == 'stopped' and receipt['inputRecords'] == 0 and receipt['outputRecords'] == 0
    print('PASS unavailable/forbidden/expired/missing logs remain not-read; wrong attempt refuses before acknowledgement', flush=True)

    tasks = [{'id': 'transform', 'job': 'batch-red', 'label': 'Transform rows', 'status': 'failure',
              'start': '2024-02-01T08:00:10Z', 'end': '2024-02-01T08:00:40Z',
              'declaredLimit': 'PT1M', 'limitSource': 'synthetic scheduler definition v1'},
             {'id': 'publish', 'job': 'batch-red', 'label': 'Publish results', 'status': 'skipped',
              'start': None, 'end': None, 'declaredLimit': None, 'limitSource': None}]
    after = app.submit(value_command('batchTasks', tasks, 'List<InvestigationTask>') + '\n' +
                       'ExecutionTimeline tasks:$batchTasks begin:"2024-02-01T08:00:00Z" end:"2024-02-01T08:02:00Z" title:"Synthetic batch execution" > batchTimeline')
    timeline = app.read(app.ready('batchTimeline', after))
    assert timeline['view'] == 'span-timeline'
    assert len(timeline['lanes'][0]['spans']) == 1 and timeline['lanes'][0]['limits'][0]['at'] == '2024-02-01T08:01:10Z'
    assert timeline['lanes'][1]['spans'] == [] and 'unavailable' in timeline['lanes'][1]['note']
    app.view('SpanTimeline', 'batchTimeline', 'batchTimelineView', timeline)
    print('PASS original LogExcerpt bridge and SpanTimeline without invented start times or timeout causes', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=DEFAULT_BINARY)
    parser.add_argument('--large', action='store_true', help='Use an explicit isolated 300M total-work profile for the 4,034-line fixture; all other limits stay at defaults.')
    args = parser.parse_args()
    binary = args.binary.resolve()
    assert binary.is_file(), f'build wes first: {binary}'
    with tempfile.TemporaryDirectory(prefix='wes-investigation-') as directory:
        root = Path(directory)
        app = Application(binary, root, args.large)
        try:
            acceptance(app, args.large)
        except BaseException:
            print((root / 'application.log').read_text(), flush=True)
            raise
        finally:
            app.close()
    print('PASS offline native investigation: no credentials, acquisitions or external chunks')


if __name__ == '__main__':
    main()
