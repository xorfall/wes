#!/usr/bin/env python3
"""Run bounded independent synthetic trials; retain failures and raw measurements."""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import random
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
CASES = ['native-only', 'calc-only', 'baseline', 'native', 'calc', 'typed', 'chain', 'fork', 'accumulate', 'slow', 'fork-slow']
MODES = ['engine', 'storage', 'journal', 'durable']

def command(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT/'target/release/examples/measure-streams')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--events', type=int, default=128)
    parser.add_argument('--payload-bytes', type=int, default=256)
    parser.add_argument('--delay-us', type=int, default=1000)
    parser.add_argument('--trials', type=int, default=3)
    parser.add_argument('--modes', nargs='+', choices=MODES, default=MODES)
    parser.add_argument('--cases', nargs='+', choices=CASES, default=CASES)
    parser.add_argument('--smoke', action='store_true')
    args = parser.parse_args()
    if args.smoke:
        args.events, args.trials = 32, 1
    assert 1 <= args.events <= 20000 and 1 <= args.trials <= 10
    assert 0 <= args.payload_bytes <= 16384 and 0 <= args.delay_us <= 10000
    binary = args.binary.resolve(strict=True)
    files = [ROOT/'crates/app/examples/measure-streams.rs', *sorted(Path(__file__).parent.glob('*.wes')), Path(__file__)]
    metadata = {'kind':'environment', 'git_head':command('git','rev-parse','HEAD'),
        'os':platform.system(), 'release':platform.release(), 'machine':platform.machine(),
        'rust':command('rustc','--version'), 'profile':'release (caller must build with --release)',
        'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),
        'source_sha256':{str(p.relative_to(ROOT)):hashlib.sha256(p.read_bytes()).hexdigest() for p in files},
        'events':args.events, 'trials':args.trials, 'payload_bytes':args.payload_bytes,
        'delay_us':args.delay_us, 'worker_threads':4, 'finite_slots':4, 'seed':20260926,
        'warmup':'one 32-event engine/baseline process; every measured process has fresh workspace/storage'}
    if platform.system() == 'Darwin':
        metadata['cpu'] = command('sysctl','-n','machdep.cpu.brand_string')
        metadata['memory_bytes'] = int(command('sysctl','-n','hw.memsize'))
    subprocess.run([str(binary),'engine','baseline','32',str(args.payload_bytes),'0'], check=True, stdout=subprocess.DEVNULL, timeout=90)
    jobs = [(trial, mode, case) for trial in range(args.trials) for mode in args.modes for case in args.cases]
    random.Random(metadata['seed']).shuffle(jobs)
    failures = 0
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open('w') as out:
        out.write(json.dumps(metadata)+'\n'); out.flush()
        for trial, mode, case in jobs:
            cmd = [str(binary),mode,case,str(args.events),str(args.payload_bytes),str(args.delay_us)]
            if platform.system() == 'Darwin': cmd = ['/usr/bin/time','-l',*cmd]
            completed = subprocess.run(cmd, capture_output=True, text=True, timeout=90)
            try:
                row = json.loads(completed.stdout)
            except (ValueError, TypeError):
                row = {'success':False, 'error':'measurement process did not return JSON', 'stderr':completed.stderr[-2000:]}
            row.update(kind='trial', trial=trial, case=case, mode=mode, exit_code=completed.returncode)
            assert row.get('case') == case and row.get('mode') == mode
            rss = re.search(r'(\d+)\s+maximum resident set size', completed.stderr)
            cpu = re.search(r'([\d.]+) real\s+([\d.]+) user\s+([\d.]+) sys', completed.stderr)
            row['max_rss_bytes'] = int(rss[1]) if rss else None
            if cpu: row.update(process_real_s=float(cpu[1]), user_s=float(cpu[2]), sys_s=float(cpu[3]))
            if row.get('success'):
                assert row['count'] == args.events and row['delivered'] == args.events
                assert row['validation_ok'] and row['source_subscriptions'] == 1
                if row['branches'] == 0:
                    assert row['ready_runs'] == args.events and row['latest_matches']
                else:
                    assert row['sequence_ok'] and row['max_offered_unfinished'] <= 501
                assert row['send_us']['count'] == args.events
                assert all(v['count'] == args.events for v in row['probe_entry_us'])
                for samples in [row['send_us'], *row['probe_entry_us']]:
                    assert 0 <= samples['p50'] <= samples['p95'] <= samples['p99'] <= samples['max']
            failed = completed.returncode != 0 or not row.get('success')
            failures += int(failed)
            out.write(json.dumps(row)+'\n'); out.flush()
            print(f'{mode}/{case} trial={trial+1}: {"FAIL" if failed else "PASS"} {row.get("settled_events_per_second",0)} events/s', flush=True)
    print(f'{len(jobs)} trials, {failures} failures; {args.output}')
    return int(failures != 0)

if __name__ == '__main__':
    sys.exit(main())
