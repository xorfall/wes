#!/usr/bin/env python3
"""Exercise the reusable pure adapters with independent synthetic API bodies."""
import argparse
import copy
import json
from pathlib import Path
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
START = 2019686400
NS = START * 10**9
PROM = {'status':'success','warnings':[],'data':{'resultType':'matrix','result':[
    {'metric':{'zone':'west'},'values':[[START+60,'99'],[START+20,'NaN'],[START,'1.25']]},
    {'metric':{'zone':'east'},'values':[[str(START+10)+'.000000001','2']]},
]}}
LOKI = {'status':'success','data':{'stats':{},'resultType':'streams','result':[
    {'stream':{'zone':'west'},'values':[[str(NS+60*10**9),'excluded'],[str(NS+10*10**9+7),'z'],[str(NS),'begin']]},
    {'stream':{'zone':'east'},'values':[[str(NS+10*10**9+7),'a'],[str(NS+10*10**9+7),'a'],[str(NS-1),'before']]},
]}}

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,default=ROOT/'target/debug/wes')
    binary=parser.parse_args().binary.resolve()
    with tempfile.TemporaryDirectory(prefix='wes-adapters-') as folder:
        root=Path(folder)
        iteration=0
        def run(prom=PROM,loki=LOKI,extra='',fails=False):
            nonlocal iteration
            iteration+=1
            source=':package load path:'+json.dumps(str(HERE/'types.yaml'))+'\n'+(HERE/'adapters.wes').read_text()
            source+='\n:calc { return interval(fromEpochSeconds('+str(START)+'), fromEpochSeconds('+str(START+60)+')); } > window\n'
            # Emit an exact numeric JSON timestamp without a binary float round-trip.
            encoded_prom=json.dumps(prom).replace(json.dumps(str(START+10)+'.000000001'),str(START+10)+'.000000001')
            source+=':calc { return parseJson('+json.dumps(encoded_prom)+'); } > prom\n'
            source+=':calc { return parseJson('+json.dumps(json.dumps(loki))+'); } > loki\n'
            source+='PrometheusTimeline input:$prom window:$window id:rate title:Rate unit:/s > metric\n'
            source+='LokiTimeline input:$loki window:$window id:logs title:Logs omitted:2 coverage:$window > logs\n'+extra
            path=root/'input.wes'; path.write_text(source)
            result=subprocess.run([str(binary),'--home',str(root/f'home-{iteration}'), '--sequential','--file',str(path)],cwd=root,text=True,capture_output=True,timeout=30)
            if fails:
                assert result.returncode!=0, result.stdout+result.stderr
                return
            assert result.returncode==0, result.stdout+result.stderr
            return [json.loads(line.partition(': ')[2]) for line in result.stdout.splitlines() if line.startswith('id') and ': ' in line][-2:]
        metric,logs=run()
        samples=[s for series in metric['series'] for s in series['samples']]
        assert any(s['at'].endswith('.000000001Z') for s in samples),metric
        assert len(samples)==3 and sum(s['gap'] for s in samples)==1,metric
        assert logs['omitted']==2 and len(logs['events'])==4,logs
        assert logs['events'][0]['detail']=='begin'
        assert all(e['at'].endswith('.000000007Z') for e in logs['events'][1:]),logs
        assert len({e['id'] for e in logs['events']})==4
        reordered=copy.deepcopy(LOKI);reordered['data']['result'].reverse()
        for stream in reordered['data']['result']:stream['values'].reverse()
        assert run(loki=reordered)[1]==logs
        empty=copy.deepcopy(PROM);empty['data']['result']=[]
        empty_logs=copy.deepcopy(LOKI);empty_logs['data']['result']=[]
        assert run(empty,empty_logs)[0]['series']==[]
        assert run(empty,empty_logs)[1]['events']==[]
        invalid=copy.deepcopy(PROM);invalid['data']['result'][0]['values'].append([START,'3'])
        run(prom=invalid,fails=True)
        invalid=copy.deepcopy(PROM);invalid['status']='error';run(prom=invalid,fails=True)
        invalid=copy.deepcopy(PROM);invalid['data']['resultType']='vector';run(prom=invalid,fails=True)
        invalid=copy.deepcopy(PROM);invalid['data']['result'][0]['histograms']=[];run(prom=invalid,fails=True)
        invalid=copy.deepcopy(PROM);invalid['data']['result'][0]['values']=[[START]];run(prom=invalid,fails=True)
        invalid=copy.deepcopy(PROM);invalid['data']['result'][0]['values']=[[START,'+Inf']];run(prom=invalid,fails=True)
        invalid=copy.deepcopy(LOKI);invalid['data']['result'][0]['values']=[['bad','line']];run(loki=invalid,fails=True)
        invalid=copy.deepcopy(LOKI);invalid['data']['result'][0]['values']=[['1','line']]*10001;run(loki=invalid,fails=True)
        invalid=copy.deepcopy(LOKI);invalid['data']['result'].append(copy.deepcopy(invalid['data']['result'][0]));run(loki=invalid,fails=True)
    print('PASS typed adapters: sorted streams, unique IDs, exact nanoseconds, half-open range, empty and malformed bodies')

if __name__=='__main__':main()
