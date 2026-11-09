"""Export all same-run objects without LLVM's aggregate hash-zero masking.

The pinned LLVM can let an unused mapping hide an executed mapping from another
object (llvm/llvm-project#160248). Keep the complete object set and merged
profile; the normal production checker unions the unchanged per-object units.
"""

import json
import re
import shlex
import subprocess
import sys
from collections import Counter
from copy import deepcopy
from itertools import pairwise
from pathlib import Path


def object_commands(log):
    """Split only Cargo's recorded export, preserving profile and filter options."""
    exports = [shlex.split(line) for line in re.findall(r"Running `(.*)`", log)]
    exports = [
        args
        for args in exports
        if len(args) > 1 and Path(args[0]).stem == "llvm-cov" and args[1] == "export"
    ]
    if len(exports) != 1:
        raise ValueError("expected one recorded LLVM export")
    base, objects = [], []
    tokens = iter(exports[0])
    for token in tokens:
        if token == "-object":
            obj = next(tokens, None)
            if obj is None:
                raise ValueError("missing LLVM object path")
            objects.append(obj)
        else:
            base.append(token)
    if not objects or sum(arg.startswith("-instr-profile=") for arg in base) != 1:
        raise ValueError("expected complete objects and one merged profile")
    return [[*base, "-object", obj] for obj in objects]


def branch_records(report):
    """Keep source decisions distinct while matching copies of one function."""
    records, required, available = [], set(), set()
    names = {source["filename"] for unit in report["data"] for source in unit["files"]}
    for unit in report["data"]:
        required.update(
            (source["filename"], *branch[:4])
            for source in unit["files"]
            for branch in source.get("branches", [])
        )
        for function in unit.get("functions", []):
            if names.isdisjoint(function["filenames"]):
                continue
            starts, occurrences = {}, Counter()
            for region in function["regions"]:
                if region[7] == 0:
                    name = function["filenames"][region[5]]
                    # Optimized copies can end their first region differently.
                    starts.setdefault(name, tuple(region[:2]))
            for branch in function.get("branches", []):
                name = function["filenames"][branch[6]]
                if name not in names:
                    continue
                if name not in starts:
                    raise ValueError("missing LLVM function source region")
                span = (name, *branch[:4])
                key = (*span, *starts[name], occurrences[span])
                occurrences[span] += 1
                records.append((key, branch[4:6]))
                available.add(span)
    # A hash-zero object can lack metadata supplied by another same-run object.
    if available != required:
        raise ValueError("missing or inconsistent LLVM function branch metadata")
    return records


def measurement_keys(report):
    """Identify every executable line and distinct decision, including zero hits."""
    keys = set()
    for unit in report["data"]:
        for source in unit["files"]:
            name = source["filename"]
            for segment, following in pairwise(source.get("segments", [])):
                if segment[3] and not segment[5] and segment[:2] != following[:2]:
                    end = following[0] - (following[1] == 1)
                    keys.update(
                        ("line", name, line) for line in range(segment[0], end + 1)
                    )
    keys.update(("branch", *key) for key, _counts in branch_records(report))
    return keys


def aggregate_keys(original, combined):
    """Recover only uniquely supported aggregate identities, never output counters."""

    def mapping(function):
        return (
            function["name"],
            tuple(function["filenames"]),
            tuple(tuple(region[:4] + region[5:]) for region in function["regions"]),
        )

    metadata = {}
    for unit in combined["data"]:
        for function in unit.get("functions", []):
            if branches := function.get("branches"):
                shape = tuple(tuple(branch[:4] + branch[6:]) for branch in branches)
                metadata.setdefault(mapping(function), {})[shape] = branches
    # LLVM can omit aggregate function branches while retaining their file spans.
    # Supplement a private key-only view; raw aggregate and per-object data stay intact.
    baseline = deepcopy(original)
    for unit in baseline["data"]:
        for function in unit.get("functions", []):
            if not function.get("branches"):
                candidates = metadata.get(mapping(function), {})
                if len(candidates) > 1:
                    raise ValueError(
                        "ambiguous LLVM aggregate function branch metadata"
                    )
                if candidates:
                    function["branches"] = next(iter(candidates.values()))
    return measurement_keys(baseline)


def reconcile(original, reports):
    """Retain all object records and reject a changed measurement denominator."""
    combined = {**original, "data": []}
    for report in reports:
        if any(report[key] != original[key] for key in ["type", "version"]):
            raise ValueError("LLVM report formats differ")
        combined["data"].extend(report["data"])
    if measurement_keys(combined) != aggregate_keys(original, combined):
        raise ValueError("per-object coverage changed the measured line/branch keyset")
    return combined


def main(command):
    """Run the ordinary report, then export every object with its exact profile."""
    output = Path(command[command.index("--output-path") + 1])
    completed = subprocess.run(
        [*command, "--verbose"], capture_output=True, text=True, check=True
    )
    output.with_suffix(".commands.log").write_text(completed.stderr, encoding="utf-8")
    commands = object_commands(completed.stderr)
    original = json.loads(output.read_text(encoding="utf-8"))
    reports = (
        json.loads(
            subprocess.run(args, capture_output=True, text=True, check=True).stdout
        )
        for args in commands
    )
    combined = reconcile(original, reports)
    output.write_text(json.dumps(combined), encoding="utf-8")
    print(
        f"Coverage export: {len(commands)} objects, one profile, unchanged measurement keys"
    )


if __name__ == "__main__":
    main(sys.argv[1:])
