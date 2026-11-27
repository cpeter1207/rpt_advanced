#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Exercise the Rust metadata loader in one isolated, test-owned Asterisk PID."""

import json
import os
import re
import socket
import subprocess
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from asterisk_working_store import WorkingStore
from echolink_fixture import EchoDirect


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
        store = WorkingStore(root)
        logfile = root / "console.log"

        def cli(command: str) -> str:
            return subprocess.run(
                ["asterisk", "-C", str(config), "-rx", command],
                check=True,
                capture_output=True,
                text=True,
                timeout=30,
            ).stdout

        def wait_network_full(offset: int) -> None:
            deadline = time.monotonic() + 5
            marker = b"rpt_fixture ready RadioPlusAdvanced/network-full\n"
            with logfile.open("rb") as log:
                while True:
                    log.seek(offset)
                    if marker in log.read():
                        return
                    if time.monotonic() >= deadline:
                        raise RuntimeError(
                            "Rust radio did not deliver 30 hardware-paced frames"
                        )
                    time.sleep(0.02)

        with logfile.open("w", encoding="utf-8") as output:
            process = subprocess.Popen(
                ["asterisk", "-f", "-C", str(config)],
                stdin=subprocess.DEVNULL,
                env=store.environment,
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
                store.start()
                store.station("1000", "network-echo")
                assert "unknown node 1000" in cli("rpt_advanced link status 1000")
                assert "unknown node 1000" in cli("rpt link status 1000")
                previously_disabled = store.disabled_revision("1000")
                store.enabled("1000", reject=True)
                cli("module reload app_rpt_advanced.so")
                assert "Running" in cli("module show like app_rpt_advanced")
                assert "unknown node 1000" in cli("rpt_advanced link status 1000")
                store.restore_disabled("1000", previously_disabled)
                store.enabled("1000", False)
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
                store.start()
                store.station("full", "network-full")
                full = store.identity("full")
                store.enabled("full")
                cli("module reload app_rpt_advanced.so")
                wait_network_full(0)
                assert f"{full} has no active links" in cli(
                    f"rpt_advanced link status {full}"
                )
                channels = cli("core show channels concise")
                assert "RadioPlusAdvanced/network-full" in channels, channels
                store.edit(
                    "PUT",
                    store.ports["full"] + "interface/",
                    {"configuration": {"audio": {"device_identifier": "3-0"}}},
                    reject=True,
                )
                cli("module reload app_rpt_advanced.so")
                retained = cli("core show channels concise")
                assert "RadioPlusAdvanced/network-full" in retained, retained
                assert "RadioPlusAdvanced/network-replacement" not in retained, retained
                assert f"{full} has no active links" in cli(f"rpt link status {full}")
                assert "invalid DTMF digit X" in cli(
                    f"rpt_advanced command {full} *720X"
                )
                store.profile(
                    "full", "interface/", {"audio": {"device_identifier": "3-8"}}
                )
                cli("module reload app_rpt_advanced.so")
                with store.status_stream(full) as subscriber:
                    opcode, payload = subscriber.frame()
                    initial = json.loads(payload)
                    assert opcode == 1 and initial["type"] == "snapshot"
                    assert initial["seq"] == "1"
                    assert full in initial["data"]["status"]["ports"]
                    with ThreadPoolExecutor(max_workers=4) as pool:
                        requests = [
                            pool.submit(cli, f"rpt_advanced link status {full}")
                            for _ in range(60)
                        ]
                        for _ in range(12):
                            cli("module reload app_rpt_advanced.so")
                            subscriber.ping()
                        for request in requests:
                            assert "rpt_advanced:" in request.result()
                    assert "RadioPlusAdvanced/network-full" in cli(
                        "core show channels concise"
                    )
                    assert process.pid == original_pid and process.poll() is None
                    cli("module unload app_rpt_advanced.so")
                    subscriber.assert_drained()
                assert "0 modules loaded" in cli("module show like app_rpt_advanced")
                assert process.pid == original_pid and process.poll() is None
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
                    store.start()
                    assert f"{full} has no active links" in cli(
                        f"rpt_advanced link status {full}"
                    )
                    cli("module unload app_rpt_advanced.so")
                    assert "RadioPlusAdvanced/" not in cli("core show channels concise")
                store.ready_offset = logfile.stat().st_size
                cli("module load app_rpt_advanced.so")
                store.start()
                wait_network_full(store.ready_offset)
                store.enabled("full", False)
                with EchoDirect() as echo:
                    for _ in range(32):
                        with (
                            socket.socket(type=socket.SOCK_DGRAM) as a,
                            socket.socket(type=socket.SOCK_DGRAM) as c,
                        ):
                            a.bind(("127.0.0.1", 0))
                            port = a.getsockname()[1]
                            if port == 65535:
                                continue
                            try:
                                c.bind(("127.0.0.1", port + 1))
                            except OSError:
                                continue
                            break
                    else:
                        raise RuntimeError("no adjacent local UDP pair")
                    fields = {
                        "enabled": True,
                        "callsign": "K1ABC-R",
                        "node_number": 654321,
                        "name": "Before",
                        "directory_servers": [echo.host],
                        "transport": "direct",
                        "audio_port": port,
                        "bind_address": "127.0.0.1",
                        "timing": {"directory_refresh_ms": 500},
                        "credentials": {"password": "fixture-value"},
                    }
                    account = store.edit(
                        "POST",
                        store.ports["1000"] + "protocols/echolink/",
                        {"configuration": fields},
                    )
                    store.enabled("1000")
                    station = store.identity("1000")

                    def status():
                        return cli(f"rpt_advanced link status {station}")

                    assert b"Before" in echo.requests.get(timeout=10)

                    def wait(predicate):
                        deadline = time.monotonic() + 10
                        while not predicate():
                            assert time.monotonic() < deadline
                            time.sleep(0.03)

                    wait(lambda: "fresh=1" in status())

                    def attached():
                        return re.search(
                            r"(?m)^\s*3123456: \S+ .*rx-missing=", status()
                        )

                    def attach():
                        echo.connect(port)
                        return attached()

                    sequence = 0

                    def keyed_tone(hz):
                        nonlocal sequence
                        offset = logfile.stat().st_size
                        marker = f"rpt_fixture echo keyed marker={hz} blocks=4".encode()
                        deadline = time.monotonic() + 10
                        with logfile.open("rb") as log:
                            log.seek(offset)
                            while time.monotonic() < deadline:
                                echo.voice(port, sequence, hz)
                                sequence += 1
                                time.sleep(0.08)
                                for line in log:
                                    if marker in line:
                                        return log.tell()
                        raise AssertionError(f"missing keyed Echo {hz} Hz: {status()}")

                    wait(attach)
                    keyed_tone(950)
                    keyed_tone(1450)
                    fields.pop("credentials")
                    fields["name"] = "After"
                    store.edit("PUT", account, {"configuration": fields})
                    cli("module reload app_rpt_advanced.so")
                    assert b"After" in echo.requests.get(timeout=10)
                    wait(attach)
                    after_key = keyed_tone(950)
                    # Direct UDP has no proxy TCP EOF. Withdraw the exact account,
                    # preserving the station owner, and observe the real link and PTT retire.
                    store.edit("DELETE", account)

                    def released():
                        with logfile.open("rb") as log:
                            log.seek(after_key)
                            edges = re.findall(
                                rb"rpt_fixture echo ptt=(\d+) remote=", log.read()
                            )
                        return edges and edges[-1] == b"0" and not attached()

                    wait(released)
                    assert "Running" in cli("module show like app_rpt_advanced")
                    cli("module unload app_rpt_advanced.so")
                    assert process.pid == original_pid and process.poll() is None
                    assert "RadioPlusAdvanced/" not in cli("core show channels concise")
                    records = re.findall(
                        r"rpt_fixture RadioPlusAdvanced/network-echo ticks=(\d+) writes=(\d+) nonzero=(\d+) early=(\d+) keys=(\d+) unkeys=(\d+)",
                        logfile.read_text(encoding="utf-8", errors="replace"),
                    )
                    assert (
                        len(records) == 1
                        and int(records[0][2]) > 0
                        and int(records[0][4]) > 0
                    ), records
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
