#!/usr/bin/env python3
"""Execute one suite from a reviewed impact plan, without shell interpolation."""
import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess

from impact import Model, ROOT, SUITES


def commands(suite, plan, model):
    if plan.get("version") != 1 or not set(plan["rust"]) <= model.crates.keys() or not set(plan["suites"]) <= SUITES:
        raise ValueError("Invalid execution plan")
    if plan.get("full") and (set(plan["rust"]) != model.crates.keys() or set(plan["suites"]) != SUITES):
        raise ValueError("A full plan must contain every suite and package")
    if suite == "rust":
        if not plan["rust"]:
            return []
        argv = ["cargo", "test", "--workspace", "--locked"]
        for package in sorted(model.crates.keys() - set(plan["rust"])):
            argv += ["--exclude", package]
        return [(ROOT, argv + ["--", "--test-threads=4"])]
    if suite not in plan["suites"]:
        return []
    if suite == "gui":
        return [(ROOT, ["npm", "run", "typecheck"]),
                (ROOT / "gui", ["npx", "--no-install", "vitest", "run"]),
                (ROOT, ["npm", "run", "build"]),
                (ROOT, ["npm", "run", "view:build", "--workspace", "wes-gui"])]
    if suite == "extractor":
        return [(ROOT / "tools/describe", ["go", "test", "./..."]),
                (ROOT / "tools/describe", ["go", "vet", "./..."])]
    if suite == "compiler":
        return [(ROOT, ["node", "--test", "tools/desktop-build.test.mjs"]),
                (ROOT, ["node", "--test", "tools/view-package/compiler.test.mjs"]),
                (ROOT, ["node", "--test", "tools/view-toolchain/export.test.mjs"])]
    if suite == "examples":
        scripts = ["quickstart", "api-import/readme", "editable-api-draft", "schema-provenance",
                   "api-workflow", "prometheus-workspace", "terminal", "assistant", "view-packages", "view-instances", "openapi-import", "ci-investigation"]
        result = []
        for name in scripts:
            argv = ["python3", f"examples/{name}/check.py"]
            if name in {"view-packages", "view-instances", "ci-investigation"}:
                argv += ["--binary", str(ROOT / "target/debug/wes")]
            result.append((ROOT, argv))
        result.insert(6, (ROOT, ["python3", "examples/prometheus-workspace/run.py", "--check"]))
        return result
    raise ValueError("Unknown suite")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("suite", choices=["rust", *sorted(SUITES)])
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--plan", type=Path)
    source.add_argument("--plan-env")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    plan = json.loads(args.plan.read_text(encoding="utf-8") if args.plan else os.environ[args.plan_env])
    for directory, argv in commands(args.suite, plan, Model()):
        print(f"[{directory.relative_to(ROOT) or '.'}] {shlex.join(argv)}", flush=True)
        if not args.dry_run:
            subprocess.run(argv, cwd=directory, check=True)


if __name__ == "__main__":
    main()
