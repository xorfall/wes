#!/usr/bin/env python3
"""Check actual declarations and retained wire envelopes in a temporary home."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=Path("target/debug/wes"))
    binary = parser.parse_args().binary.resolve()
    with tempfile.TemporaryDirectory(prefix="wes-declared-tones-") as directory:
        root = Path(directory)
        home = root / "home"
        for name in ["types.yaml", "sample.wes"]:
            shutil.copyfile(HERE / name, root / name)

        def run(*args, succeeds=True):
            result = subprocess.run(
                [str(binary), "--home", str(home), "--json", *args],
                cwd=root, capture_output=True, text=True, timeout=30,
            )
            assert (result.returncode == 0) == succeeds, result.stdout + result.stderr
            return [json.loads(line.split(": ", 1)[1])
                    for line in result.stdout.splitlines()
                    if line.startswith("id") and ": " in line]

        results = run("--file", "sample.wes")
        rows = next(value for value in results if isinstance(value, list) and len(value) == 3)
        assert [row["status"] for row in rows] == ["ready", "failed", "unknown"]
        description = next(value for value in results
                           if isinstance(value, dict) and value.get("name") == "ServiceStatus")
        assert description["display"]["enumTones"] == {"ready": "ok", "failed": "bad", "unknown": "dim"}
        assert "enumTones" not in description["constraints"]
        assert description["digest"].startswith("sha256:")

        # Only inspect this check's owned files. CLI data output intentionally stays plain data.
        retained = [json.loads(path.read_text()) for path in (home / "values").rglob("*.json")]
        retained = [value for value in retained if value.get("format") == "wes.value"]
        declared = [value for value in retained
                    if value.get("meta", {}).get("contract", {}).get("name") == "List<ServiceRow>"]
        assert {len(value["data"]["value"]) for value in declared} == {0, 3}
        expected = None
        for value in declared:
            assert value["version"] == 2 and value["meta"]["version"] == 1
            meta = value["meta"]
            domain = meta["fields"]["/e/f:status"]
            assert domain["members"] == ["ready", "failed", "unknown"]
            assert domain["tones"] == description["display"]["enumTones"]
            assert domain["total"] == 3 and domain["complete"] and not meta["truncated"]
            assert len(json.dumps(meta, separators=(",", ":")).encode()) <= 64 * 1024
            expected = meta
        run("--command", ':workspace save "tones"')
        (root / "types.yaml").unlink()
        run("--command", ':workspace load "tones"')
        restored = run("--command", ":inspect type:ServiceStatus")
        assert restored == [description]
        # Loading uses captured declarations and retained values; it does not read the removed file.
        assert any(json.loads(path.read_text()).get("meta") == expected
                   for path in (home / "values").rglob("*.json"))

        (root / "invalid.yaml").write_text(
            "version: 2\ntypes: {Bad: {base: Text, enum: [ready], "
            "display: {enumTones: {ready: inherit}}}}\n"
        )
        run("--command", ":package load path:invalid.yaml", succeeds=False)
    print("PASS declared tones, empty-list domains, retained metadata and captured save/reopen")


if __name__ == "__main__":
    main()
