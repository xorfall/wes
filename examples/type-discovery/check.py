#!/usr/bin/env python3
"""Run the real example in an isolated home; no external service."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    binary = parser.parse_args().binary.resolve()
    with tempfile.TemporaryDirectory(prefix="wes-type-discovery-") as directory:
        root = Path(directory)
        for name in ["types.yaml", "inspect.wes"]:
            shutil.copyfile(HERE/name, root/name)

        def run(*args):
            result = subprocess.run([str(binary), "--home", str(root/"home"), *args],
                                    cwd=root, capture_output=True, text=True, timeout=30)
            assert result.returncode == 0, result.stdout + result.stderr
            return [json.loads(line.split(": ", 1)[1]) for line in result.stdout.splitlines()
                    if line.startswith("id") and ": " in line]

        results = run("--file", str(root/"inspect.wes"))
        types = next(v for v in results if isinstance(v, list))
        assert {r["name"] for r in types} >= {"Int", "Customer", "Positive", "List", "Map", "Option", "Iter"}
        customer = next(v for v in results if isinstance(v, dict) and v.get("name") == "Customer")
        fields = {f["name"]: f for f in customer["fields"]}
        assert fields["note"]["optional"] and not fields["id"]["optional"]
        assert fields["id"]["contract"]["constraints"]["min"] == "1"
        generic = next(v for v in results if isinstance(v, dict) and v.get("kind") == "list")
        assert generic["element"] == customer
        constructor = next(v for v in results if isinstance(v, dict) and v.get("kind") == "constructor")
        assert constructor["parameters"] == ["K", "V"]
        assert run("--command", ':workspace save "discovery"') == []
        (root/"types.yaml").unlink()
        assert run("--command", ':workspace load "discovery"') == []
        # Reopening does not replay either package I/O or saved query workers.
        assert "type" in run("--command", ":help inspect > inspect_help\n:calc { return $inspect_help.invocation.parameters.map(p => p.name); }")[0]
        assert run("--command", ":refresh $customer_type") == [customer]
        assert run("--command", ":inspect type:Customer") == [customer]
    print("PASS actual type-discovery example, metadata, save/reopen held, explicit refresh")


if __name__ == "__main__":
    main()
