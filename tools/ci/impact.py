#!/usr/bin/env python3
"""Conservative test impact planning using stdlib only (Python 3.11+)."""
from __future__ import annotations

import argparse
import fnmatch
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
SUITES = {"gui", "extractor", "compiler", "examples"}


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], stderr=subprocess.PIPE)


def matches(path, patterns):
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns)


class Model:
    def __init__(self, root=ROOT):
        self.root = Path(root).resolve()
        self.policy = tomllib.loads((self.root / "ci/tests.toml").read_text(encoding="utf-8"))
        if self.policy.get("version") != 1 or set(self.policy["suites"]) != SUITES:
            raise ValueError("Unsupported test policy or suite names")
        workspace = tomllib.loads((self.root / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]
        self.crates, manifests = {}, {}
        for member in workspace["members"]:
            # This workspace uses explicit member paths. Reject syntax we cannot
            # interpret instead of quietly constructing an incomplete graph.
            if any(c in member for c in "*?[") or member.startswith("/") or ".." in PurePosixPath(member).parts:
                raise ValueError("Test graph requires explicit in-repository Cargo members")
            manifest = self.root / member / "Cargo.toml"
            if manifest.is_symlink() or not manifest.resolve().is_relative_to(self.root):
                raise ValueError("Cargo member manifest escapes the repository")
            doc = tomllib.loads(manifest.read_text(encoding="utf-8"))
            name = doc["package"]["name"]
            if name in self.crates or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]*", name):
                raise ValueError("Duplicate or invalid Cargo member")
            directory = (self.root / member).resolve()
            targets = [doc.get("lib", {}), *doc.get("bin", []), *doc.get("test", []),
                       *doc.get("example", []), *doc.get("bench", [])]
            explicit_paths = [target["path"] for target in targets if "path" in target]
            explicit_paths += [doc["package"][key] for key in ("build", "readme")
                               if isinstance(doc["package"].get(key), str)]
            if any(not (directory / path).resolve().is_relative_to(directory) for path in explicit_paths):
                raise ValueError("External Cargo target/build/readme path requires a test-policy update")
            self.crates[name], manifests[name] = member, doc
        if workspace.get("exclude"):
            raise ValueError("Cargo excludes require an explicit test-policy update")
        self.reverse = {name: set() for name in self.crates}
        by_path = {(self.root / member).resolve(): name for name, member in self.crates.items()}
        for owner, doc in manifests.items():
            tables = [doc, *doc.get("target", {}).values()]
            for table in tables:
                for kind in ("dependencies", "build-dependencies", "dev-dependencies"):
                    for alias, dep in table.get(kind, {}).items():
                        if not isinstance(dep, dict):
                            continue
                        base = self.root / self.crates[owner]
                        if dep.get("workspace"):
                            dep = workspace.get("dependencies", {})[alias]
                            base = self.root
                        if isinstance(dep, dict) and "path" in dep:
                            path = (base / dep["path"]).resolve()
                            if path not in by_path:
                                raise ValueError("Local path dependency is outside the workspace test graph")
                            self.reverse[by_path[path]].add(owner)
        for rule in self.policy["rules"]:
            if not set(rule["rust"]) <= self.crates.keys() or not set(rule["suites"]) <= SUITES:
                raise ValueError("Unknown test-policy dependency")
        for suite in self.policy["suites"].values():
            if not set(suite["rust_inputs"]) <= self.crates.keys():
                raise ValueError("Unknown suite Rust dependency")
        # Literal include_str!/include_bytes! inputs are compiler dependencies
        # even when stored outside a crate. Runtime and generated inputs belong
        # in the explicit cross-language rules above.
        self.included = {}
        for owner, member in self.crates.items():
            for source in (self.root / member).rglob("*.rs"):
                if source.is_symlink():
                    continue  # Git diff treats changed links as full inputs.
                text = source.read_text(encoding="utf-8")
                normal = re.findall(r'include(?:_(?:str|bytes))?!\s*\(\s*"([^"\n]+)"', text)
                raw = [m.group("path") for m in re.finditer(
                    r'include(?:_(?:str|bytes))?!\s*\(\s*r(?P<hash>\#*)"(?P<path>[^"\n]+)"(?P=hash)', text)]
                modules = re.findall(r'#\s*\[\s*path\s*=\s*"([^"\n]+)"', text)
                for included in normal + raw + modules:
                    if "\\" in included:
                        raise ValueError("Escaped include paths require an explicit test-policy update")
                    path = (source.parent / included).resolve()
                    try:
                        relative = path.relative_to(self.root).as_posix()
                    except ValueError as error:
                        raise ValueError("Included input escapes repository") from error
                    self.included.setdefault(relative, set()).add(owner)

    def closure(self, seeds):
        result = set(seeds)
        pending = list(seeds)
        while pending:
            for consumer in self.reverse[pending.pop()] - result:
                result.add(consumer)
                pending.append(consumer)
        return result

    def plan(self, files, full_reason=None):
        files = sorted(set(files))
        seeds, suites, reasons, docs = set(), set(), [], []
        for path in files:
            if path.startswith("/") or ".." in PurePosixPath(path).parts:
                full_reason = f"Invalid repository path: {path!r}"
                break
            if matches(path, self.policy["full"]):
                full_reason = f"Shared build or test-policy input: {path}"
                break
            owners = {name for name, member in self.crates.items() if path.startswith(member + "/")}
            owners |= self.included.get(path, set())
            rules = [rule for rule in self.policy["rules"] if matches(path, rule["paths"])]
            if owners or rules:
                seeds |= owners
                reasons.append(f"{path}: " + ", ".join(sorted(owners) + [r["name"] for r in rules]))
                for rule in rules:
                    seeds.update(rule["rust"])
                    suites.update(rule["suites"])
            elif matches(path, self.policy["documentation"]):
                docs.append(path)
            else:
                full_reason = f"Unclassified input: {path}"
                break
        if full_reason:
            rust, suites = set(self.crates), set(SUITES)
            reasons = [full_reason]
        else:
            rust = self.closure(seeds)
            for name, suite in self.policy["suites"].items():
                if rust.intersection(suite["rust_inputs"]):
                    suites.add(name)
        return {
            "version": 1, "full": bool(full_reason), "files": files,
            "rust": sorted(rust), "suites": sorted(suites), "reasons": reasons,
            "documentation": docs,
        }


def changed(root, base, head):
    for ref in (base, head):
        if not ref or ref.startswith("-"):
            raise ValueError("Invalid Git reference")
        git(root, "rev-parse", "--verify", ref + "^{commit}")
    records = git(root, "diff", "--name-status", "-z", "--find-renames", base, head, "--").split(b"\0")
    paths, i = [], 0
    while i < len(records) - 1:
        status = records[i].decode("ascii")
        count = 2 if status[0] in "RC" else 1
        paths.extend(p.decode("utf-8", "surrogateescape") for p in records[i + 1:i + 1 + count])
        i += 1 + count
    # Symlinks and gitlinks can redirect inputs outside a declared component.
    raw = git(root, "diff", "--raw", "-z", base, head, "--").split(b"\0")
    special = any(r.startswith(b":") and any(mode in (b"120000", b"160000") for mode in r[1:].split()[:2]) for r in raw)
    return paths, "Symlink or submodule changed" if special else None


def successful_base(root, environ):
    repository = environ["GITHUB_REPOSITORY"]
    if not re.fullmatch(r"[\w.-]+/[\w.-]+", repository):
        raise ValueError("Invalid GitHub repository")
    url = f"https://api.github.com/repos/{repository}/actions/workflows/ci.yml/runs?branch=main&status=success&per_page=100"
    request = urllib.request.Request(url, headers={
        "Authorization": "Bearer " + environ["GH_TOKEN"],
        "Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28",
    })
    with urllib.request.urlopen(request, timeout=20) as response:
        runs = json.load(response)["workflow_runs"]
    for run in runs:
        sha = run["head_sha"]
        if run["event"] not in ("push", "schedule", "workflow_dispatch") or not re.fullmatch(r"[0-9a-f]{40}", sha):
            continue
        try:
            git(root, "merge-base", "--is-ancestor", sha, "HEAD")
            return sha
        except subprocess.CalledProcessError:
            continue
    raise ValueError("No successful ancestor CI run")


def event_changes(root, event_name, event, environ):
    try:
        if event_name == "pull_request":
            merge_base = git(root, "merge-base", event["pull_request"]["base"]["sha"], "HEAD").decode().strip()
            base = successful_base(root, environ)
            git(root, "merge-base", "--is-ancestor", base, merge_base)
        elif event_name == "push" and event.get("ref") == "refs/heads/main":
            before = event.get("before", "")
            if event.get("forced") or not before or set(before) == {"0"}:
                return [], "New branch or rewritten push history"
            git(root, "merge-base", "--is-ancestor", before, "HEAD")
            # Diff from the last green run, not merely this push's before SHA:
            # queued/cancelled/failed pushes must remain in the impact range.
            base = successful_base(root, environ)
        else:
            return [], f"Full verification event: {event_name}"
        paths, reason = changed(root, base, "HEAD")
        return paths, reason
    except (KeyError, ValueError, OSError, subprocess.CalledProcessError):
        return [], "Reliable comparison base unavailable; full verification"


def emit(plan, output):
    execution = {k: plan[k] for k in ("version", "full", "rust", "suites")}
    if output:
        suites, rust = set(plan["suites"]), set(plan["rust"])
        flags = {
            "client": bool(suites & {"gui", "extractor"}),
            "engine": bool(rust or suites & {"compiler", "examples"}),
            "gui": "gui" in suites, "extractor": "extractor" in suites,
            "compiler": "compiler" in suites, "examples": "examples" in suites,
            "rust": bool(rust), "desktop": "wes-desktop" in rust,
            "go_build": bool(rust & {"wes", "wes-desktop"}) or bool(suites & {"compiler", "examples"}),
        }
        with Path(output).open("a", encoding="utf-8") as stream:
            stream.write("plan=" + json.dumps(execution, separators=(",", ":")) + "\n")
            for name, value in flags.items():
                stream.write(f"{name}={str(value).lower()}\n")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with Path(summary).open("a", encoding="utf-8") as stream:
            stream.write("## Test impact plan\n\nFull: " + str(plan["full"]) + "\n\n")
            stream.write("```json\n" + json.dumps(plan, indent=2) + "\n```\n")
    print(json.dumps(plan, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--files", nargs="*")
    source.add_argument("--base")
    source.add_argument("--full", action="store_true")
    source.add_argument("--github", action="store_true")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--worktree", action="store_true")
    parser.add_argument("--github-output")
    args = parser.parse_args()
    model = Model()
    reason = "Explicit full verification" if args.full else None
    files = args.files or []
    if args.github:
        files, reason = event_changes(ROOT, os.environ["GITHUB_EVENT_NAME"], json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text(encoding="utf-8")), os.environ)
    elif args.base:
        try:
            files, reason = changed(ROOT, args.base, args.head)
            if args.worktree:
                data = git(ROOT, "diff", "--name-only", "-z", args.head, "--")
                data += git(ROOT, "ls-files", "--others", "--exclude-standard", "-z")
                files += [p.decode("utf-8", "surrogateescape") for p in data.split(b"\0") if p]
                # Raw diff includes modes/links from both index and worktree.
                raw = git(ROOT, "diff", "--raw", args.head, "--")
                if b"120000" in raw or b"160000" in raw:
                    reason = "Worktree symlink or submodule changed"
        except (ValueError, subprocess.CalledProcessError):
            files, reason = [], "Comparison base unavailable; full verification"
    emit(model.plan(files, reason), args.github_output)


if __name__ == "__main__":
    main()
