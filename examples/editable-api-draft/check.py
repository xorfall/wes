#!/usr/bin/env python3
"""Validate deterministic extraction and explicit edits with the application validator."""
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
with tempfile.TemporaryDirectory(prefix='wes-editable-draft-') as directory:
    home = Path(directory)/'home'
    def action(request):
        run = subprocess.run([str(ROOT/'target/debug/wes'), '--home', str(home), '--api-request', '-'], input=json.dumps(request), capture_output=True, text=True, timeout=30)
        assert run.returncode == 0, run.stdout + run.stderr
        return json.loads(run.stdout)
    source = ROOT/'examples/api-import/openapi.json'
    generated = subprocess.run([str(ROOT/'tools/describe/wes-extract'), '-draft', '-provider', 'items', '-from', str(source)], capture_output=True, text=True, timeout=30)
    assert generated.returncode == 0, generated.stderr
    envelope = json.loads(generated.stdout)
    text = json.dumps(envelope['draft'], indent=2)
    checked = action({'action':'validateDraft', 'text':text})
    assert checked['hash'] == hashlib.sha256(text.encode()).hexdigest()
    assert checked['valid'], checked
    assert envelope['source']['sha256'] == hashlib.sha256(source.read_bytes()).hexdigest()
    # The parser's Union output must also be admitted by Wes's actual native validator.
    for keyword in ('anyOf', 'oneOf'):
        union_source = json.loads(source.read_text())
        union_source['components']['schemas']['Item'] = {keyword: [{'type':'string'}, {'type':'integer'}]}
        generated = subprocess.run([str(ROOT/'tools/describe/wes-extract'), '-draft', '-provider', 'items', '-from', '-'], input=json.dumps(union_source), capture_output=True, text=True, timeout=30)
        assert generated.returncode == 0, generated.stderr
        result = action({'action':'validateDraft','text':json.dumps(json.loads(generated.stdout)['draft'])})
        assert result['valid'], result
    incomplete = (HERE/'incomplete-draft.json').read_text()
    assert not action({'action':'validateDraft','text':incomplete})['valid']
    ready = (HERE/'completed-draft.json').read_text()
    assert action({'action':'validateDraft','text':ready})['valid']
    assert not action({'action':'validateDescriptor','text':ready})['valid']
    assert not action({'action':'validateDraft','text':'{\n'})['valid']
    relaxed = envelope['draft']
    response = relaxed['operations'][0]['responses'][0]
    response['type'] = 'Unknown'
    response['mediaType'] = None
    uncertain = action({'action':'validateDraft','text':json.dumps(relaxed)})
    assert uncertain['valid'], uncertain
    assert any(d['code']=='DRAFT_MEDIA_UNKNOWN' and d['severity']=='warning' for d in uncertain['diagnostics'])
    response['mediaType'] = 'not a media type'
    assert not action({'action':'validateDraft','text':json.dumps(relaxed)})['valid']
print('PASS OpenAPI draft, manual edits, strict transport, source digest and unknown contracts')
