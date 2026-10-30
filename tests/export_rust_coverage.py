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


def measurement_keys(report):
    """Identify every executable line and branch coordinate, including zero hits."""
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
            keys.update(
                ("branch", name, *branch[:4]) for branch in source.get("branches", [])
            )
    return keys


def reconcile(original, reports):
    """Retain all object records and reject a changed measurement denominator."""
    combined = {**original, "data": []}
    for report in reports:
        if any(report[key] != original[key] for key in ["type", "version"]):
            raise ValueError("LLVM report formats differ")
        combined["data"].extend(report["data"])
    if measurement_keys(combined) != measurement_keys(original):
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
