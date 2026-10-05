#!/usr/bin/env python3
"""Real stdio acceptance, isolated files, no model account or live workspace."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=Path("target/debug/wes"))
    args = parser.parse_args()
    requests = json.loads(Path(__file__).with_name("requests.json").read_text())
    wire = b"".join((json.dumps(request, ensure_ascii=False) + "\n").encode() for request in requests)
    env = dict(os.environ)
    for name in ("WES_BRIDGE_URL", "WES_BRIDGE_TOKEN", "WES_MCP_METRICS_DIR"):
        env.pop(name, None)
    # Deliberately no bridge: tool replies must report unavailability, not success.
    def run(extra):
        result = subprocess.run([str(args.binary.resolve()), "--assistant-mcp"], input=wire,
                                capture_output=True, env={**env, **extra}, timeout=15)
        assert result.returncode == 0, result.stderr.decode()
        replies = [json.loads(line) for line in result.stdout.splitlines()]
        return result, sorted(replies, key=lambda reply: str(reply["id"]))

    with tempfile.TemporaryDirectory(prefix="wes-agent-metrics-") as home:
        directory = Path(home) / "reports"
        baseline, plain = run({})
        assert not directory.exists()
        result, measured = run({"WES_MCP_METRICS_DIR": str(directory)})
        assert plain == measured
        files = list(directory.glob("mcp-*.json"))
        assert len(files) == 1
        text = files[0].read_text()
        report = json.loads(text)
        assert report["schema"] == "wes.mcp-metrics.v1"
        assert report["state"] == "closed"
        assert report["totals"]["input_bytes"] == len(wire)
        assert report["totals"]["output_bytes"] == len(result.stdout) == len(baseline.stdout)
        assert report["totals"]["messages"] == report["totals"]["completed"] == len(requests)
        assert report["totals"]["outcomes"] == {"ok": 3, "no_reply": 1, "unavailable": 2, "protocol_error": 1}
        assert report["groups"]["discovery"]["messages"] == 2
        assert report["groups"]["tools"]["messages"] == 2
        assert report["buckets"]["tools/cancel"]["outcomes"] == {"unavailable": 1}
        assert "PRIVATE" not in text and "synthetic-fixture" not in text
        # Reconnection creates another bounded report rather than overwriting a session.
        run({"WES_MCP_METRICS_DIR": str(directory)})
        assert len(list(directory.glob("mcp-*.json"))) == 2
        # A bad export destination cannot change protocol behavior or add stdout noise.
        invalid = Path(home) / "file"
        invalid.write_text("fixture")
        failure, replies = run({"WES_MCP_METRICS_DIR": str(invalid)})
        assert replies == plain
        assert b"metrics unavailable" in failure.stderr
    print("PASS: exact MCP wire bytes, outcomes, privacy, isolated exports and unchanged replies")


if __name__ == "__main__":
    main()
