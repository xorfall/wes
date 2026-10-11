#!/usr/bin/env python3
"""Exercise native declarative providers in an isolated home; no accounts or services."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/wes")
    args = parser.parse_args()
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix="wes-declarative-providers-") as temporary:
        project = Path(temporary)
        for name in ["demo.yaml", "main.wes", "environments.yaml"]:
            shutil.copyfile(HERE / name, project / name)
        built = subprocess.run(["rustc", "--edition=2024", str(ROOT / "examples/support/echo.rs"),
                                "-o", str(project / "example-echo.bin")],
                               capture_output=True, text=True, timeout=60)
        assert built.returncode == 0, built.stderr

        def run(home, *arguments):
            result = subprocess.run([str(binary), "--home", str(project / home), *arguments],
                                    cwd=project, capture_output=True, text=True, timeout=30)
            assert result.returncode == 0, result.stdout + result.stderr
            return [json.loads(line.partition(": ")[2]) for line in result.stdout.splitlines()
                    if line.startswith("id") and ": " in line]

        results = run("direct-home", "--file", str(project / "main.wes"))
        assert {"count": 42, "label": "literal arguments"} in results, results
        assert results[-1] == "a b $(literal) --label first --label second --verbose\n", results
        results = run("managed-home", "--env-file", str(project / "environments.yaml"),
                      "--env", "demo", "--command", 'demo records echo payload:{count:7,label:"managed"}')
        assert results[-1] == {"count": 7, "label": "managed"}, results
        for name in ["demo.yaml", "example-echo.bin"]:
            (project / name).unlink()
        assert run("direct-home", "--command", ":read $record")[-1] == {
            "count": 42, "label": "literal arguments"}
        print("PASS declarative providers: typed JSON, literal argv, optional flags, managed binding and inert replay")


if __name__ == "__main__":
    main()
