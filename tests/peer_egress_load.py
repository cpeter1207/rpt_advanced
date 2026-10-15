#!/usr/bin/env python3
"""Run the real dynamic-provider transmit path against loopback-only IAX peers.

Run inside the maintained, labeled quality container with candidate libraries
on LD_LIBRARY_PATH. Results are observations, not noisy CI performance gates.
"""

import argparse
import json
import os
import signal
import subprocess
from pathlib import Path


def positive(value):
    """Require a useful requested peer count or measured duration."""
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be positive")
    return number


def main():
    """Build and run one isolated experiment; terminate its process group on exit."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--peers", type=positive, default=1000)
    parser.add_argument("--seconds", type=positive, default=120)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    output.unlink(missing_ok=True)
    env = dict(
        os.environ,
        RPTADV_LOAD_PEERS=str(args.peers),
        RPTADV_LOAD_SECONDS=str(args.seconds),
        RPTADV_LOAD_OUTPUT=str(output),
    )
    command = [
        "cargo",
        "test",
        "--locked",
        "--release",
        "-p",
        "rptadv-product",
        "--lib",
        "egress_full_path_load",
        "--",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ]
    process = subprocess.Popen(command, env=env, start_new_session=True)
    try:
        result = process.wait()
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
    if output.exists():
        report = json.loads(output.read_text())
        report["cpu_percent_including_simulator"] = (
            report["cpu_ticks"] / os.sysconf("SC_CLK_TCK") / report["seconds"] * 100
        )
        report["host"] = dict(
            zip(("system", "node", "release", "version", "machine"), os.uname())
        )
        report["cargo_exit_code"] = result
        report["logical_cpus"] = os.cpu_count()
        output.write_text(json.dumps(report, indent=2) + "\n")
        print(
            json.dumps(
                {key: value for key, value in report.items() if key != "per_peer"},
                indent=2,
            )
        )
    raise SystemExit(result)


if __name__ == "__main__":
    main()
