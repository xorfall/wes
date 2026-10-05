#!/usr/bin/env python3
"""Run actual example files in a temporary home, with no provider invocations."""
import argparse, json, subprocess, tempfile
from pathlib import Path
HERE=Path(__file__).resolve().parent
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary',type=Path,required=True)
args=parser.parse_args();binary=args.binary.resolve()
with tempfile.TemporaryDirectory(prefix='wes-command-system-') as directory:
    home=Path(directory)/'home'
    def run(*arguments):
        result=subprocess.run([str(binary),'--home',str(home),*arguments],cwd=HERE,text=True,capture_output=True,timeout=30)
        assert result.returncode==0,result.stderr
        return [json.loads(line.split(': ',1)[1]) for line in result.stdout.splitlines() if line.startswith("id") and ": " in line]
    run('--env-file',str(HERE/'environments.yaml'),'--file',str(HERE/'values.wes'))
    env,environments,cells,runs,homes,help_report=run('--file',str(HERE/'discover.wes'))
    family,operation=help_report['family'],help_report['operation']
    assert family['invocation']=={'kind':'none'}
    assert {c['name'] for c in family['children']}=={'refresh','change','cancel','timeout','policy','remove'}
    assert operation['invocation']['shortForm']==':refresh $result [scope:downstream]'
    assert operation['invocation']['operands']=={'min':1,'max':1}
    assert env['name']=='demo' and isinstance(env['providers'],list)
    assert 'demo' in environments
    assert cells and all('id' in cell and 'source' not in cell for cell in cells)
    assert runs and all({'id','node','state'}<=r.keys() for r in runs)
    assert len(homes)==1
    page,metadata=run('--file',str(HERE/'read.wes'))
    assert page=={'data':[20,30],'offset':1,'total':4,'hasMore':True},page
    assert metadata['port']=='data' and 'rows' not in metadata
    assert run('--command',':help refresh > shortcut_help\n:calc { return {help: $shortcut_help}; }')[0]['help']==operation
    missing=subprocess.run([str(binary),'--home',str(home),'--command',':env use demo'],capture_output=True,text=True)
    assert missing.returncode!=0 and 'quoted' in missing.stderr,missing.stderr
print('PASS command-system: actual environment recipe, layered help, typed queries, retained read page, metadata-only inspection, shortcut parity and precise rejection')
