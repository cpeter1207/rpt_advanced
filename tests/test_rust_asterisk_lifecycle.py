#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Exercise the Rust metadata loader in one isolated, test-owned Asterisk PID."""

import os
import re
import subprocess
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path


def main() -> None:
    """Load, reject an enabled replacement, reload, unload, and load again."""
    modules = Path(os.environ["RPT_TEST_MODULE_DIR"]).resolve()
    with tempfile.TemporaryDirectory(prefix="rpt-rust-lifecycle-") as temporary:
        root = Path(temporary)
        config = root / "asterisk.conf"
        config.write_text(
            "[directories]\n"
            + "".join(
                f"{name} => {root}\n"
                for name in (
                    "astetcdir",
                    "astrundir",
                    "astlogdir",
                    "astvarlibdir",
                    "astdbdir",
                    "astspooldir",
                )
            )
            + f"astmoddir => {modules}\n",
            encoding="utf-8",
        )
        (root / "modules.conf").write_text(
            "[modules]\nautoload=no\nload=app_rpt_advanced.so\n", encoding="utf-8"
        )
        node = root / "rpt_advanced.conf"
        node.write_text("[1000]\nnode_enabled=no\n", encoding="utf-8")
        logfile = root / "console.log"

        def cli(command: str) -> str:
            return subprocess.run(
                ["asterisk", "-C", str(config), "-rx", command],
                check=True,
                capture_output=True,
                text=True,
                timeout=30,
            ).stdout

        with logfile.open("w", encoding="utf-8") as output:
            process = subprocess.Popen(
                ["asterisk", "-f", "-C", str(config)],
                stdin=subprocess.DEVNULL,
                stdout=output,
                stderr=subprocess.STDOUT,
            )
            original_pid = process.pid
            try:
                deadline = time.monotonic() + 30
                while True:
                    if process.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError("Rust module did not become usable")
                    if (root / "asterisk.ctl").exists():
                        listing = cli("module show like app_rpt_advanced")
                        if "Running" in listing and "Not Running" not in listing:
                            break
                    time.sleep(0.1)
                assert "unknown node 1000" in cli("rpt_advanced link status 1000")
                assert "unknown node 1000" in cli("rpt link status 1000")
                node.write_text("[1000]\nradio_channel=missing\n", encoding="utf-8")
                cli("module reload app_rpt_advanced.so")
                assert "Running" in cli("module show like app_rpt_advanced")
                assert "unknown node 1000" in cli("rpt_advanced link status 1000")
                node.write_text("[1000]\nnode_enabled=no\n", encoding="utf-8")
                cli("module reload app_rpt_advanced.so")
                for _ in range(2):
                    cli("module unload app_rpt_advanced.so")
                    assert "0 modules loaded" in cli(
                        "module show like app_rpt_advanced"
                    )
                    cli("module load app_rpt_advanced.so")
                    assert "Running" in cli("module show like app_rpt_advanced")
                    assert process.pid == original_pid and process.poll() is None
                cli("module load chan_rpt_fixture.so")
                active = "[full]\nradio_channel=network-full\nfull_duplex=yes\n"
                node.write_text(active, encoding="utf-8")
                cli("module reload app_rpt_advanced.so")
                deadline = time.monotonic() + 5
                while (
                    "rpt_fixture ready RadioPlusAdvanced/network-full"
                    not in logfile.read_text(encoding="utf-8", errors="replace")
                ):
                    if time.monotonic() >= deadline:
                        raise RuntimeError(
                            "Rust radio did not deliver 30 hardware-paced frames"
                        )
                    time.sleep(0.02)
                assert "full has no active links" in cli(
                    "rpt_advanced link status full"
                )
                channels = cli("core show channels concise")
                assert "RadioPlusAdvanced/network-full" in channels, channels
                node.write_text(
                    "[full]\nradio_channel=network-replacement\n"
                    "[courtesy full receiver]\ninput=receiver\ntone_sequence=invalid\n",
                    encoding="utf-8",
                )
                cli("module reload app_rpt_advanced.so")
                retained = cli("core show channels concise")
                assert "RadioPlusAdvanced/network-full" in retained, retained
                assert "RadioPlusAdvanced/network-replacement" not in retained, retained
                assert "full has no active links" in cli("rpt link status full")
                assert "invalid DTMF digit X" in cli("rpt_advanced command full *720X")
                node.write_text(active, encoding="utf-8")
                cli("module reload app_rpt_advanced.so")
                with ThreadPoolExecutor(max_workers=4) as pool:
                    requests = [
                        pool.submit(cli, "rpt_advanced link status full")
                        for _ in range(60)
                    ]
                    for _ in range(12):
                        cli("module reload app_rpt_advanced.so")
                    for request in requests:
                        assert "rpt_advanced:" in request.result()
                assert "RadioPlusAdvanced/network-full" in cli(
                    "core show channels concise"
                )
                cli("module unload app_rpt_advanced.so")
                assert "RadioPlusAdvanced/" not in cli("core show channels concise")
                records = re.findall(
                    r"rpt_fixture RadioPlusAdvanced/network-full ticks=(\d+) writes=(\d+) "
                    r"nonzero=(\d+) early=(\d+) keys=(\d+) unkeys=(\d+)",
                    logfile.read_text(encoding="utf-8", errors="replace"),
                )
                assert len(records) == 1, records
                ticks, writes, nonzero, _, keys, unkeys = map(int, records[0])
                assert ticks >= writes >= 30 and nonzero > 0 and keys == unkeys, records
                for _ in range(8):
                    cli("module load app_rpt_advanced.so")
                    assert "full has no active links" in cli(
                        "rpt_advanced link status full"
                    )
                    cli("module unload app_rpt_advanced.so")
                    assert "RadioPlusAdvanced/" not in cli("core show channels concise")
                cli("module unload chan_rpt_fixture.so")
                print(f"Rust module same-PID lifecycle passed: pid={original_pid}")
            except BaseException:
                print(logfile.read_text(encoding="utf-8", errors="replace"))
                raise
            finally:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=10)


if __name__ == "__main__":
    main()
