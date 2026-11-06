#!/usr/bin/env python3
"""Turn Criterion and allocation output into a GitHub Actions job summary."""

import datetime
import json
import os
import platform
import subprocess
from pathlib import Path

def human_time(nanoseconds: float) -> str:
    if nanoseconds < 1_000:
        return f"{nanoseconds:.2f} ns"
    if nanoseconds < 1_000_000:
        return f"{nanoseconds / 1_000:.3f} µs"
    if nanoseconds < 1_000_000_000:
        return f"{nanoseconds / 1_000_000:.3f} ms"
    return f"{nanoseconds / 1_000_000_000:.3f} s"

def target_root() -> Path:
    configured = os.environ.get("CARGO_TARGET_DIR")
    if configured:
        path = Path(configured)
        return path if path.is_absolute() else Path.cwd() / path
    return Path("target")

def rust_version() -> str:
    try:
        return subprocess.check_output(
            ["rustc", "--version"], text=True, stderr=subprocess.DEVNULL
        ).strip()
    except (OSError, subprocess.SubprocessError):
        return "unknown"

def main() -> None:
    criterion = target_root() / "criterion"
    rows = []
    for benchmark in criterion.glob("**/new/benchmark.json"):
        estimates = benchmark.with_name("estimates.json")
        if not estimates.is_file():
            continue
        try:
            metadata = json.loads(benchmark.read_text(encoding="utf-8"))
            estimate = json.loads(estimates.read_text(encoding="utf-8"))
            median = estimate["median"]["point_estimate"]
        except (OSError, KeyError, ValueError):
            continue
        name = metadata.get("full_id") or metadata.get("title") or benchmark.parent.parent.name
        rows.append((name, median))

    lines = ["## Martite benchmarks", ""]
    lines.append(f"- OS: `{platform.platform()}`")
    lines.append(f"- Rust: `{rust_version()}`")
    lines.append(
        f"- Date: {datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%d')}"
    )
    commit = os.environ.get("GITHUB_SHA", "")[:7]
    if commit:
        lines.append(f"- Commit: `{commit}`")
    lines.append("- Command: `cargo bench -p martite-bench`")
    lines.append("")

    if rows:
        lines.append("| Benchmark | Median |")
        lines.append("| --- | ---: |")
        for name, median in sorted(rows):
            lines.append(f"| `{name}` | {human_time(median)} |")
    else:
        lines.append("No Criterion results were found.")

    allocations = criterion / "allocations.md"
    if allocations.is_file():
        lines.extend(["", "## Runtime allocations", "", allocations.read_text(encoding="utf-8").strip()])

    summary = "\n".join(lines) + "\n"
    destination = os.environ.get("GITHUB_STEP_SUMMARY")
    if destination:
        with open(destination, "a", encoding="utf-8") as output:
            output.write(summary)
    print(summary, end="")

if __name__ == "__main__":
    main()
