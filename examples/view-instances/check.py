#!/usr/bin/env python3
"""Prerequisite: cargo build -p wes; node tools/view-build.mjs; npm --prefix gui run build.
Run: python3 examples/view-instances/check.py --binary target/debug/wes
Expected: the Metric keeps its identity after a rebind; Timeline selection is linked to RangeSummary.
Run the scenario's commands as separate GUI submissions; drag a Timeline range to update the summary.
All values are synthetic; no providers or credentials are needed. The home is temporary.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument("--binary", type=Path, required=True)
binary = parser.parse_args().binary.resolve()
with tempfile.TemporaryDirectory(prefix="wes-view-instances-") as directory:
    root = Path(directory)
    compiler = Path(__file__).resolve().parents[2]/"tools/view-package/index.mjs"
    source = Path(__file__).resolve().parents[1]/"view-packages/source/range-summary"
    compiled = subprocess.run(["node", str(compiler), "build", str(source), str(root/"range-summary.wes-view.json")], env={**os.environ,"WES_VIEW_CONTRACT_TOOL":str(binary.with_name("wes-view-build"))}, capture_output=True, text=True, timeout=60)
    assert compiled.returncode == 0, compiled.stdout + compiled.stderr
    scenario = root/"scenario.yaml"
    scenario.write_text(Path(__file__).with_name("scenario.yaml").read_text())
    result = subprocess.run([str(binary), "--home", str(Path(directory)/"home"), "--test",
                             str(scenario)],
                            capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, result.stdout + result.stderr
    report = json.loads(result.stdout)
    assert len(report["steps"]) == 20, report
    assert all(step["status"] == "passed" for step in report["steps"]), report
print("PASS actual view create/connect/rebind/output-link workflow")
