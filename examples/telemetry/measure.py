#!/usr/bin/env python3
"""Fresh-process, synthetic macOS comparisons; no live application access."""
import argparse, json, pathlib, re, statistics, subprocess
p=argparse.ArgumentParser();p.add_argument('--repetitions',type=int,default=5);p.add_argument('--output',required=True);p.add_argument('--binary',type=pathlib.Path);a=p.parse_args()
assert a.repetitions>=3,'At least three repetitions are required.'
root=pathlib.Path(__file__).resolve().parents[2];binary=a.binary or root/'target/release/examples/measure-telemetry'
scenarios=[('operations',100000),('engine',100),('web',500)];modes=['off','basic','diagnostic'];samples=[]
for scenario,n in scenarios:
    subprocess.run([str(binary),'off',scenario,str(n)],check=True,capture_output=True)
    for repeat in range(a.repetitions):
        order=modes if repeat%2==0 else list(reversed(modes))
        for mode in order:
            result=subprocess.run(['/usr/bin/time','-l',str(binary),mode,scenario,str(n)],check=True,capture_output=True,text=True)
            value=json.loads(result.stdout)
            assert value['iterations']==n
            counts=sum(sum(o[1] for o in metric['outcomes']) for metric in value['status']['metrics']['operations'])
            assert counts==0 if mode=='off' else counts>0
            cpu=re.search(r'([\d.]+) user\s+([\d.]+) sys',result.stderr)
            rss=re.search(r'(\d+)\s+maximum resident set size',result.stderr)
            assert cpu and rss,result.stderr
            value.update(repeat=repeat,cpu_seconds=float(cpu[1])+float(cpu[2]),peak_rss_bytes=int(rss[1]))
            samples.append(value)
summary=[]
for scenario,_ in scenarios:
    for mode in modes:
        rows=[s for s in samples if s['scenario']==scenario and s['mode']==mode]
        summary.append(dict(scenario=scenario,mode=mode,samples=len(rows),**{k:statistics.median(r[k] for r in rows) for k in ['wall_seconds','cpu_seconds','peak_rss_bytes']}))
report={'schema':1,'limitations':'Synthetic backend only; RSS is process high-water, not physical footprint or UI memory. No universal overhead threshold.','summary':summary,'samples':samples}
pathlib.Path(a.output).write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(summary,indent=2))
