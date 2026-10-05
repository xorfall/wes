#!/usr/bin/env python3
"""One always-running required check for both selected and omitted CI jobs."""
import json
import os


def check_results(jobs):
    if jobs["plan"]["result"] != "success":
        raise ValueError("Test planning failed or was cancelled")
    for output, job in [("client", "client-and-extractor"), ("engine", "engine-and-examples")]:
        selected = jobs["plan"]["outputs"][output]
        if selected not in ("true", "false"):
            raise ValueError("Missing or invalid job selection")
        expected = "success" if selected == "true" else "skipped"
        if jobs[job]["result"] != expected:
            raise ValueError(f"{job}: expected {expected}, got {jobs[job]['result']}")


if __name__ == "__main__":
    check_results(json.loads(os.environ["CI_RESULTS"]))
    print("All planned test jobs passed; omitted jobs match the impact plan.")
