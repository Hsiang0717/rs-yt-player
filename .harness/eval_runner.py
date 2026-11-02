#!/usr/bin/env python3
"""Evaluation Runner for rs-yt-player Harness Optimization.

Runs cargo test, parses test results, extracts compiler and test gradients,
and outputs standardized JSON metrics.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import time
from pathlib import Path


def run_cargo_tests(timeout_sec: int = 60) -> tuple[int, int, float, list[str]]:
    start_time = time.perf_counter()
    sanitized_gradients: list[str] = []

    cmd = ["cargo", "test", "--", "--nocapture"]

    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout_sec,
            check=False,
        )
        duration_ms = round((time.perf_counter() - start_time) * 1000, 2)

        stdout = proc.stdout or ""
        stderr = proc.stderr or ""
        combined = stdout + "\n" + stderr

        passed = 0
        failed = 0

        # Look for "test result: ok. X passed; Y failed"
        for line in combined.splitlines():
            if "test result:" in line:
                parts = line.split()
                for i, p in enumerate(parts):
                    if p == "passed;":
                        passed = int(parts[i - 1])
                    elif p == "failed;":
                        failed = int(parts[i - 1])

        if proc.returncode != 0:
            if failed == 0 and ("error[E" in combined or "error:" in combined):
                # Compilation failure
                for line in combined.splitlines():
                    if "error[E" in line or "error:" in line:
                        sanitized_gradients.append(line.strip()[:140])
                failed = max(1, len(sanitized_gradients))
            else:
                for line in combined.splitlines():
                    if "FAILED" in line or "panicked at" in line:
                        sanitized_gradients.append(line.strip()[:140])

        return passed, failed, duration_ms, sanitized_gradients[:5]

    except subprocess.TimeoutExpired:
        duration_ms = round(timeout_sec * 1000, 2)
        return 0, 10, duration_ms, [f"Timeout exceeded ({timeout_sec}s)"]


def get_git_diff_lines() -> int:
    try:
        res = subprocess.run(
            ["git", "diff", "--shortstat"],
            capture_output=True,
            text=True,
            check=False,
        )
        out = res.stdout.strip()
        insertions = 0
        deletions = 0
        for part in out.split(","):
            if "insertion" in part:
                insertions = int("".join(filter(str.isdigit, part)))
            elif "deletion" in part:
                deletions = int("".join(filter(str.isdigit, part)))
        return insertions + deletions
    except Exception:
        return 0


def compute_composite_loss(
    failed_count: int,
    total_count: int,
    latency_ms: float,
    diff_lines: int,
) -> float:
    fail_rate = failed_count / max(1, total_count)
    latency_norm = min(1.0, latency_ms / 30000.0)
    complexity_norm = min(1.0, diff_lines / 200.0)

    w_fail = 0.70
    w_lat = 0.20
    w_diff = 0.10

    raw_loss = (w_fail * fail_rate) + (w_lat * latency_norm) + (w_diff * complexity_norm)
    return round(raw_loss, 4)


def main() -> None:
    parser = argparse.ArgumentParser(description="rs-yt-player Eval Runner")
    parser.add_argument("--epoch", type=int, default=0)
    parser.add_argument("--config", type=str, default=".harness/config.json")
    args = parser.parse_args()

    passed, failed, latency_ms, gradients = run_cargo_tests()
    total = passed + failed
    pass_rate = round(passed / max(1, total), 4)
    diff_lines = get_git_diff_lines()

    loss = compute_composite_loss(failed, total, latency_ms, diff_lines)

    output = {
        "epoch": args.epoch,
        "composite_loss": loss,
        "pass_rate": pass_rate,
        "failed_count": failed,
        "passed_count": passed,
        "latency_ms": latency_ms,
        "diff_lines": diff_lines,
        "sanitized_gradients": gradients,
    }
    print(json.dumps(output, indent=2))


if __name__ == "__main__":
    main()
