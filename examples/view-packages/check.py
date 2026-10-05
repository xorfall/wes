#!/usr/bin/env python3
"""Prerequisite: cargo build -p wes; node tools/view-build.mjs; npm --prefix gui run build.
Run: python3 examples/view-packages/check.py --binary target/debug/wes
Expected: actual example lists/inspects packages and produces the typed Metric input.
No providers, credentials or external services are used; the home is temporary.
"""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument("--binary", type=Path, required=True)
binary = parser.parse_args().binary.resolve()
with tempfile.TemporaryDirectory(prefix="wes-view-packages-") as directory:
    result = subprocess.run([str(binary), "--home", str(Path(directory)/"home"), "--file",
                             str(Path(__file__).with_name("main.wes").resolve())],
                            capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, result.stdout + result.stderr
    values = [json.loads(line.split(": ", 1)[1]) for line in result.stdout.splitlines()
              if line.startswith("id") and ": " in line]
    assert {"Timeline", "TimelineGroup", "Metric"} <= set(values[0])
    definitions = {value["name"]: value for value in values if isinstance(value, dict) and "digest" in value}
    assert definitions["Timeline"]["inputModes"] == ["value"]
    assert definitions["Timeline"]["outputScope"] == "instance"
    assert definitions["Timeline"]["outputs"]["selection"]["shared"] is True
    assert definitions["Metric"]["execution"] == "none"
    assert all(definition["artifact"] for definition in definitions.values())
    assert values[-1] == {"view": "metric", "value": 1250}
print("PASS actual view-package discovery and finite renderer input")
