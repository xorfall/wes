#!/usr/bin/env python3
"""Run isolated fixed-duration stream trials with bounded RSS sampling on macOS."""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import random
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
CASES = ['native-only', 'calc-only', 'baseline', 'native', 'calc', 'typed', 'chain', 'chain-single-calc', 'fork', 'accumulate', 'slow', 'fork-slow']
MODES = ['engine', 'storage', 'journal', 'durable']


def command(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def run(binary, mode, case, seconds, payload, delay, ending):
    # Temporary output files avoid pipe-capacity deadlocks; sampling state is O(seconds).
    with tempfile.TemporaryFile(mode='w+') as stdout, tempfile.TemporaryFile(mode='w+') as stderr:
        process = subprocess.Popen([str(binary), mode, case, str(seconds), str(payload), str(delay), ending],
                                   cwd=ROOT, stdout=stdout, stderr=stderr)
        started = time.monotonic()
        samples = []
        timed_out = False
        try:
            while process.poll() is None:
                elapsed = time.monotonic() - started
                if elapsed > seconds + 75:
                    timed_out = True
                    process.kill()
                    break
                stat = subprocess.run(['ps', '-o', 'rss=', '-p', str(process.pid)],
                                      capture_output=True, text=True, timeout=5)
                if stat.returncode == 0 and stat.stdout.strip():
                    samples.append({'elapsed_s': elapsed, 'rss_bytes': int(stat.stdout.strip()) * 1024})
                time.sleep(1)
            code = process.wait(timeout=10)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=10)
        stdout.seek(0)
        stderr.seek(0)
        try:
            row = json.load(stdout)
        except (ValueError, TypeError):
            row = {'success': False, 'error': 'missing measurement JSON'}
        row.update(exit_code=code, external_timeout=timed_out, rss_samples=samples,
                   sampled_peak_rss_bytes=max((x['rss_bytes'] for x in samples), default=None),
                   process_wall_s=time.monotonic() - started, stderr=stderr.read()[-4000:])
        return row


def validate(row, case, mode, ending):
    if not row.get('success'):
        return False
    assert row['case'] == case and row['mode'] == mode
    assert row['physically_idle'] and row['validation_ok'] and row['source_subscriptions'] == 1 and row['sent'] > 0
    assert row['send']['count'] == row['sent']
    assert not row['timed_out'] and not row['external_timeout'] and row['exit_code'] == 0
    assert not row['recording_blocked'] and not row['log_unconfirmed'] and not row['log_capture_failures']
    assert all(ending == 'cancel' and e['code'] == 'RUN003' for e in row['errors'])
    assert not row['storage_failures']
    assert row['log_entries'] <= 10000
    if row['branches']:
        assert row['sequence_ok'] and row['max_offered_unfinished'] <= 501
        if ending == 'complete':
            assert row['probe_completed'] == row['sent']
            assert all(h['count'] == row['sent'] for h in row['probe_entry'])
    elif ending == 'complete':
        assert row['latest_matches']
    if ending == 'cancel':
        assert row['cancel_ms'] is not None and row['probe_completed'] <= row['sent']
    assert all(not h['recording_blocked'] and not h['errors'] for h in row['heartbeats'])
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT/'target/release/examples/measure-sustained-streams')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--seconds', type=int, default=30)
    parser.add_argument('--payload-bytes', type=int, default=256)
    parser.add_argument('--delay-us', type=int, default=1000)
    parser.add_argument('--trials', type=int, default=2)
    parser.add_argument('--cases', nargs='+', choices=CASES, default=CASES)
    parser.add_argument('--modes', nargs='+', choices=MODES, default=['engine', 'durable'])
    parser.add_argument('--ending', choices=['complete', 'cancel'], default='complete')
    parser.add_argument('--smoke', action='store_true')
    args = parser.parse_args()
    assert platform.system() == 'Darwin', 'RSS sampler uses macOS ps units'
    if args.smoke:
        args.seconds, args.trials = 1, 1
    assert 1 <= args.seconds <= 120 and 1 <= args.trials <= 5
    assert 0 <= args.payload_bytes <= 16384 and 0 <= args.delay_us <= 10000
    binary = args.binary.resolve(strict=True)
    files = [ROOT/'crates/app/examples/measure-sustained-streams.rs',
             *sorted((ROOT/'examples/stream-performance').glob('*.wes')),
             *sorted(Path(__file__).parent.glob('*.wes')), Path(__file__)]
    metadata = {'kind': 'environment', 'git_head': command('git', 'rev-parse', 'HEAD'),
                'git_status': command('git', 'status', '--short'),
                'os': platform.platform(), 'cpu': command('sysctl', '-n', 'machdep.cpu.brand_string'),
                'memory_bytes': int(command('sysctl', '-n', 'hw.memsize')),
                'rust': command('rustc', '--version'), 'profile': 'release (caller must build with --release)',
                'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                'source_sha256': {str(p.relative_to(ROOT)): hashlib.sha256(p.read_bytes()).hexdigest() for p in files},
                'seconds': args.seconds, 'trials': args.trials, 'payload_bytes': args.payload_bytes,
                'delay_us': args.delay_us, 'ending': args.ending, 'worker_threads': 4, 'finite_slots': 4,
                'seed': 20260926, 'warmup': 'none; fresh process/workspace/stores for each trial',
                'rss': 'whole benchmark process, sampled once per second; not allocator live-byte accounting'}
    jobs = [(trial, mode, case) for trial in range(args.trials) for mode in args.modes for case in args.cases]
    random.Random(metadata['seed']).shuffle(jobs)
    failures = 0
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open('w') as out:
        out.write(json.dumps(metadata)+'\n')
        out.flush()
        for trial, mode, case in jobs:
            row = run(binary, mode, case, args.seconds, args.payload_bytes, args.delay_us, args.ending)
            row.update(kind='trial', trial=trial)
            try:
                valid = validate(row, case, mode, args.ending)
            except (AssertionError, KeyError, TypeError) as error:
                row['runner_validation_error'] = repr(error)
                valid = False
            row['runner_valid'] = valid
            failures += not valid
            out.write(json.dumps(row)+'\n')
            out.flush()
            print(f'{mode}/{case} {args.ending} trial={trial+1}: {"PASS" if valid else "FAIL"} '
                  f'{row.get("settled_events_per_second", 0):.0f} events/s', flush=True)
    print(f'{len(jobs)} trials, {failures} failures; {args.output}')
    return int(failures != 0)


if __name__ == '__main__':
    sys.exit(main())
