#!/usr/bin/env python3
"""Turn Criterion and allocation output into a GitHub Actions job summary."""

import datetime
import json
import os
import platform
import subprocess
import sys
from pathlib import Path

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")

def human_time(nanoseconds: float) -> str:
    if nanoseconds < 1_000:
        return f"{nanoseconds:.2f} ns"
    if nanoseconds < 1_000_000:
        return f"{nanoseconds / 1_000:.3f} µs"
    if nanoseconds < 1_000_000_000:
        return f"{nanoseconds / 1_000_000:.3f} ms"
    return f"{nanoseconds / 1_000_000_000:.3f} s"

def read_change(benchmark: Path) -> float | None:
    estimates = benchmark.parent.parent / "change" / "estimates.json"
    if not estimates.is_file():
        return None
    try:
        change = json.loads(estimates.read_text(encoding="utf-8"))
        return change["median"]["point_estimate"]
    except (OSError, KeyError, ValueError):
        return None


def read_estimate(path: Path) -> float | None:
    if not path.is_file():
        return None
    try:
        estimates = json.loads(path.read_text(encoding="utf-8"))
        return estimates["median"]["point_estimate"]
    except (OSError, KeyError, ValueError):
        return None


def format_change(change: float | None) -> str:
    return f"{change * 100:+.1f}%" if change is not None else "—"


def optimizer_summary(criterion: Path) -> list[str]:
    """Report the optimizer target's lowered-versus-optimized pair.

    This is intentionally separate from the general runtime comparison. The
    workflow saves a `before` directory for the baseline revision; a full
    benchmark run has no baseline and only gets the normal table.
    """
    root = criterion / "optimizer"
    if not root.is_dir():
        return []

    rows = {}
    for benchmark in sorted(root.iterdir()):
        if not benchmark.is_dir():
            continue
        before = read_estimate(benchmark / "before" / "estimates.json")
        after = read_estimate(benchmark / "new" / "estimates.json")
        if before is None and after is None:
            continue
        rows[benchmark.name] = (before, after)
    if not rows or all(before is None for before, _ in rows.values()):
        return []

    lines = [
        "",
        "## Optimizer delta",
        "",
        "| Benchmark | Baseline | Candidate | Change |",
        "| --- | ---: | ---: | ---: |",
    ]
    for name in sorted(rows):
        before, after = rows[name]
        change = None
        if before is not None and after is not None and before:
            change = (after - before) / before
        lines.append(
            "| `optimizer/{name}` | {before} | {after} | {change} |".format(
                name=name,
                before=human_time(before) if before is not None else "—",
                after=human_time(after) if after is not None else "—",
                change=format_change(change),
            )
        )

    lowered = rows.get("runtime_lowered")
    optimized = rows.get("runtime_optimized")
    if lowered and optimized and all(
        value is not None for value in (*lowered, *optimized)
    ):
        baseline_gain = (
            (optimized[0] - lowered[0]) / lowered[0] if lowered[0] else None
        )
        candidate_gain = (
            (optimized[1] - lowered[1]) / lowered[1] if lowered[1] else None
        )
        lines.extend(
            [
                "",
                "| Optimizer gain | Baseline | Candidate |",
                "| --- | ---: | ---: |",
                "| lowered -> optimized | {baseline} | {candidate} |".format(
                    baseline=format_change(baseline_gain),
                    candidate=format_change(candidate_gain),
                ),
            ]
        )
    return lines

def read_allocations(path: Path) -> dict[str, int]:
    rows = {}
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError:
        return rows
    for line in lines:
        parts = [part.strip() for part in line.split("|")]
        if len(parts) < 4 or not parts[1].startswith("runtime/"):
            continue
        try:
            rows[parts[1]] = int(parts[2])
        except ValueError:
            continue
    return rows

def allocation_summary(criterion: Path) -> list[str]:
    allocations = criterion / "allocations.md"
    if not allocations.is_file():
        return []
    candidate = read_allocations(allocations)
    baseline = read_allocations(criterion / "allocations-baseline.md")
    if not candidate or not baseline:
        return ["", "## Runtime allocations", "", allocations.read_text(encoding="utf-8").strip()]

    lines = [
        "",
        "## Runtime allocations",
        "",
        "| Benchmark | Baseline | Candidate | Delta |",
        "| --- | ---: | ---: | ---: |",
    ]
    for name in sorted(candidate.keys() & baseline.keys()):
        delta = candidate[name] - baseline[name]
        formatted = f"{delta:+d}" if delta else "0"
        lines.append(f"| `{name}` | {baseline[name]} | {candidate[name]} | {formatted} |")
    return lines

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
        rows.append((name, median, read_change(benchmark)))

    lines = ["## Martite benchmarks", ""]
    lines.append(f"- OS: `{platform.platform()}`")
    lines.append(f"- Rust: `{rust_version()}`")
    lines.append(
        f"- Date: {datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%d')}"
    )
    commit = os.environ.get("GITHUB_SHA", "")[:7]
    if commit:
        lines.append(f"- Commit: `{commit}`")
    baseline = os.environ.get("BASELINE_SHA", "")[:7]
    candidate = os.environ.get("CANDIDATE_SHA", "")[:7]
    if baseline and candidate:
        lines.append(f"- Baseline: `{baseline}`")
        lines.append(f"- Candidate: `{candidate}`")
    lines.append("- Command: `cargo bench -p martite-bench`")
    lines.append("")

    has_change = any(change is not None for _, _, change in rows)
    if rows and has_change:
        lines.append("| Benchmark | Median | Change |")
        lines.append("| --- | ---: | ---: |")
        for name, median, change in sorted(rows):
            formatted = f"{change * 100:+.1f}%" if change is not None else "—"
            lines.append(f"| `{name}` | {human_time(median)} | {formatted} |")
    elif rows:
        lines.append("| Benchmark | Median |")
        lines.append("| --- | ---: |")
        for name, median, _ in sorted(rows):
            lines.append(f"| `{name}` | {human_time(median)} |")
    else:
        lines.append("No Criterion results were found.")

    lines.extend(optimizer_summary(criterion))
    lines.extend(allocation_summary(criterion))

    summary = "\n".join(lines) + "\n"
    destination = os.environ.get("GITHUB_STEP_SUMMARY")
    if destination:
        with open(destination, "a", encoding="utf-8") as output:
            output.write(summary)
    print(summary, end="")

if __name__ == "__main__":
    main()
