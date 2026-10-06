#!/usr/bin/env python3
"""Exercise direct JSON/YAML import, explicit destinations and frozen replay offline."""
import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import subprocess
import tempfile
from threading import Thread

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/wes")
    args = parser.parse_args()
    binary = args.binary.resolve()
    extractor = ROOT / "tools/describe/wes-extract"
    assert binary.is_file() and extractor.is_file(), "Build wes and tools/describe/wes-extract first."
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            requests.append(self.path)
            body = json.dumps(source if self.path == "/openapi.json" else {"ok": True}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix="wes-direct-openapi-") as directory:
            base = Path(directory).resolve()
            endpoint = f"http://127.0.0.1:{server.server_port}"
            source = {"openapi": "3.0.3", "info": {"title": "Synthetic", "version": "1"}, "servers": [{"url": "http://127.0.0.1:1"}], "security": [], "paths": {"/health": {"get": {"operationId": "health", "responses": {"200": {"description": "OK", "content": {"application/json": {"schema": {"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"]}}}}}}}}}
            (base / "api.json").write_text(json.dumps(source))
            (base / "api.yaml").write_text("""openapi: 3.1.0
info: {title: Synthetic, version: '1'}
security: []
paths:
  /health:
    get:
      operationId: health
      responses:
        '200':
          description: OK
          content:
            application/json:
              schema: {type: object, properties: {ok: {type: boolean}}, required: [ok]}
""")

            def run(home, *arguments, expected=0, input=None):
                result = subprocess.run([str(binary), "--home", str(home), *arguments], input=input, cwd=base, capture_output=True, text=True, timeout=60)
                assert result.returncode == expected, result.stdout + result.stderr
                return result

            def action(home, request):
                return json.loads(run(home, "--api-request", "-", input=json.dumps(request)).stdout)

            def configure(home, compiler):
                if not home.exists():
                    run(home, "--command", ":calc { return 1; }")
                status = action(home, {"action": "status"})
                action(home, {"action": "configure", "expectedRevision": status["revision"], "settings": {"localDirectory": str(home / "api-library"), "extractor": str(compiler)}})

            for extension in ("json", "yaml"):
                home = base / f"home-{extension}"
                configure(home, extractor)
                help_result = run(home, "--json", "--command", ":help import openapi")
                help_data = json.loads(help_result.stdout.partition(": ")[2])
                assert next(p for p in help_data["invocation"]["parameters"] if p["name"] == "endpoint")["required"]
                missing = run(home, "--command", ":import openapi file:absent.yaml", expected=1)
                assert "endpoint" in missing.stderr
                before = len(requests)
                imported = run(home, "--json", "--sequential", "--command", f':import openapi file:api.{extension} endpoint:"{endpoint}" as:demo\ndemo health > result')
                values = [json.loads(line.partition(": ")[2]) for line in imported.stdout.splitlines() if line.startswith("id")]
                assert values[-1]["status"] == 200 and values[-1]["body"] == {"ok": True}, imported.stdout
                assert values[-1]["validation"]["state"] == "validated", imported.stdout
                assert len(requests) == before + 1

            home_url = base / "home-url"
            configure(home_url, extractor)
            before = len(requests)
            run(home_url, "--command", f':import openapi url:"{endpoint}/openapi.json" endpoint:"{endpoint}" as:remote')
            assert requests[before:] == ["/openapi.json"], "import called an API operation"
            run(home_url, "--command", ":calc { return 1; }")
            assert len(requests) == before + 1, "replay downloaded OpenAPI source"

            # Reopen with both original source and usable converter unavailable.
            (base / "api.json").unlink()
            refused = base / "refused-extractor"
            refused.write_text("#!/bin/sh\necho unexpectedly-called > compiler-called\nexit 29\n")
            refused.chmod(0o700)
            configure(base / "home-json", refused)
            before = len(requests)
            run(base / "home-json", "--command", ":calc { return 1; }")
            assert len(requests) == before, "workspace replay called API"
            run(base / "home-json", "--command", "demo health > fresh")
            assert len(requests) == before + 1 and not (base / "compiler-called").exists()

            # Environment packages use the same normalized contract and destination rules.
            home = base / "home-env"
            configure(home, extractor)
            package = {"version": 1, "targets": {"local": {"kind": "local"}}, "environments": {"lab": {"imports": {"demo": {"source": {"kind": "openapi", "file": "api.yaml"}, "bind": {"target": "local", "endpoint": endpoint}}}}}}
            (base / "environment.json").write_text(json.dumps(package))
            run(home, "--env-file", str(base / "environment.json"), "--env", "lab", "--activate-env", "--command", "demo health > result")
            assert requests[-1] == "/health"
            package["environments"]["lab"]["imports"]["demo"]["source"]["sha256"] = "0" * 64
            (base / "pinned.json").write_text(json.dumps(package))
            before = len(requests)
            run(home, "--env-file", str(base / "pinned.json"), "--env", "lab", "--command", "demo health", expected=1)
            assert len(requests) == before
            undocumented = dict(source)
            undocumented.pop("security")
            (base / "undocumented.json").write_text(json.dumps(undocumented))
            advisory = run(base / "home-yaml", "--json", "--command", f':import openapi file:undocumented.json endpoint:"{endpoint}" as:undocumented')
            assert "IMP010" in advisory.stderr and "not documented" in advisory.stderr
            bad = dict(source)
            bad["components"] = {"schemas": {"External": {"$ref": "http://127.0.0.1:1/never-fetch"}}}
            (base / "external.json").write_text(json.dumps(bad))
            before = len(requests)
            rejected = run(base / "home-yaml", "--command", f':import openapi file:external.json endpoint:"{endpoint}" as:broken', expected=1)
            assert "unsupported or invalid" in rejected.stderr and len(requests) == before
            print("PASS direct OpenAPI JSON/YAML, help/admission, explicit endpoint, typed HTTP, environment integration, unsupported refs and converter-free inert replay")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


if __name__ == "__main__":
    main()
