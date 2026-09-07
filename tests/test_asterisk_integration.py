#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""! @brief Load, reload, and unload the built module in an isolated Asterisk process."""

import os
import re
import shutil
import subprocess
import tempfile
import time
from pathlib import Path


def cli(configuration: Path, command: str) -> str:
    """! @brief Execute one CLI command against the test-owned Asterisk socket.
    @param configuration Test configuration path.
    @param command Asterisk CLI command.
    @return Captured CLI output; failures raise an exception.
    """
    return subprocess.run(
        ["asterisk", "-C", str(configuration), "-rx", command],
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    ).stdout


def audio_case(
    configuration: Path,
    radio_configuration: Path,
    logfile: Path,
    process: subprocess.Popen,
    rate: int,
    codec: str,
) -> None:
    """! @brief Exchange two radios through actual Asterisk converters and reload.
    @param configuration Isolated Asterisk configuration.
    @param radio_configuration Controller configuration to replace.
    @param logfile Test-owned diagnostic output.
    @param process Running isolated Asterisk.
    @param rate Explicit rate or zero for native auto selection.
    @param codec Requested codec, empty for native linear.
    @return None; assertions verify transport and duplex behavior.
    """
    offset = len(logfile.read_text(encoding="utf-8", errors="replace"))
    radio_configuration.write_text(
        f"[general]\nsample_rate_hz={rate}\ncodec={codec}\n"
        "[full]\nfull_duplex=yes\n"
        "[half]\nfull_duplex=no\n"
        "[identifier]\ninterval_ms=50\nmorse_text=E\n"
        "[identifier full periodic]\n"
        "[identifier half periodic]\n",
        encoding="utf-8",
    )
    cli(configuration, "module reload app_rpt_advanced.so")
    deadline = time.monotonic() + 30
    while (
        logfile.read_text(encoding="utf-8", errors="replace")[offset:].count(
            "rpt_fixture ready "
        )
        < 2
    ):
        if process.poll() is not None or time.monotonic() >= deadline:
            raise TimeoutError(f"radio exchange did not complete: {rate=} {codec=}")
        time.sleep(0.1)
    radio_configuration.write_text("", encoding="utf-8")
    cli(configuration, "module reload app_rpt_advanced.so")
    records = re.findall(
        r"rpt_fixture RadioPlusAdvanced/(full|half) ticks=(\d+) "
        r"writes=(\d+) nonzero=(\d+) early=(\d+) keys=(\d+) unkeys=(\d+)",
        logfile.read_text(encoding="utf-8", errors="replace")[offset:],
    )
    assert len(records) == 2, records
    for name, *values in records:
        ticks, writes, nonzero, early, keys, unkeys = map(int, values)
        assert writes >= 30 and ticks >= writes, records
        if not rate:
            assert ticks == writes, records
        assert nonzero > 0 and keys > 0 and unkeys == keys, records
        assert (early > 0) == (name == "full"), records
    print(f"Asterisk audio {rate=} {codec=}: {records}")


def main() -> None:
    """! @brief Verify the real module ABI and always stop the test-owned process.
    @return None; failures raise an exception.
    """
    with tempfile.TemporaryDirectory(prefix="rpt-advanced-asterisk-") as temporary:
        directory = Path(temporary)
        module_directory = Path(os.environ["RPT_TEST_MODULE_DIR"])
        configuration = directory / "asterisk.conf"
        configuration.write_text(
            "[directories]\n"
            f"astetcdir => {directory}\n"
            f"astmoddir => {module_directory}\n"
            f"astrundir => {directory}\n"
            f"astlogdir => {directory}\n"
            f"astvarlibdir => {directory}\n"
            f"astdbdir => {directory}\n"
            f"astspooldir => {directory}\n",
            encoding="utf-8",
        )
        (directory / "modules.conf").write_text(
            "[modules]\nautoload=no\nload=app_rpt_advanced.so\n", encoding="utf-8"
        )
        radio_configuration = directory / "rpt_advanced.conf"
        radio_configuration.write_text(
            Path(
                "build/stage/usr/share/doc/rpt_advanced/examples/rpt_advanced.conf"
            ).read_text(encoding="utf-8"),
            encoding="utf-8",
        )
        logfile = directory / "console.log"
        with logfile.open("w", encoding="utf-8") as output:
            process = subprocess.Popen(
                ["asterisk", "-f", "-C", str(configuration)],
                stdin=subprocess.DEVNULL,
                stdout=output,
                stderr=subprocess.STDOUT,
            )
            try:
                deadline = time.monotonic() + 90
                while not (directory / "asterisk.ctl").exists():
                    if process.poll() is not None:
                        raise RuntimeError("test Asterisk exited during startup")
                    if time.monotonic() >= deadline:
                        raise TimeoutError(
                            "test Asterisk did not create its control socket"
                        )
                    time.sleep(0.1)
                while True:
                    try:
                        listing = cli(
                            configuration, "module show like app_rpt_advanced"
                        )
                    except subprocess.CalledProcessError:
                        listing = ""
                    if (
                        "app_rpt_advanced.so" in listing
                        and "Running" in listing
                        and "Not Running" not in listing
                    ):
                        break
                    if process.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError(f"module did not start: {listing}")
                    time.sleep(0.1)
                radio_configuration.write_text(
                    "[usb]\nfull_duplex=yes\n", encoding="utf-8"
                )
                cli(configuration, "module reload app_rpt_advanced.so")
                listing = cli(configuration, "module show like app_rpt_advanced")
                assert "Running" in listing and "Not Running" not in listing, listing
                # No driver is loaded: the rejected active node must not replace
                # the original disabled configuration. Restore a valid reload
                # before testing a fresh module load in this same process.
                radio_configuration.write_text(
                    "[usb]\nnode_enabled=no\nfull_duplex=yes\n", encoding="utf-8"
                )
                cli(configuration, "module reload app_rpt_advanced.so")
                cli(configuration, "module unload app_rpt_advanced.so")
                listing = cli(configuration, "module show like app_rpt_advanced")
                assert "0 modules loaded" in listing, listing
                cli(configuration, "module load app_rpt_advanced.so")
                listing = cli(configuration, "module show like app_rpt_advanced")
                assert (
                    "Running" in listing
                    and "Not Running" not in listing
                    and process.poll() is None
                ), listing
                # The fixture is copied only into this test's staging tree; it is
                # not an install artifact and cannot access USB hardware.
                shutil.copyfile(
                    "build/chan_rpt_fixture.so",
                    module_directory / "chan_rpt_fixture.so",
                )
                cli(configuration, "module load chan_rpt_fixture.so")
                for library in ("codec_resample.so", "codec_ulaw.so"):
                    candidates = list(
                        Path("/usr/lib").glob(f"*/asterisk/modules/{library}")
                    )
                    candidates += list(Path("/usr/lib/asterisk/modules").glob(library))
                    assert candidates, f"ASL3 test image is missing {library}"
                    shutil.copyfile(candidates[0], module_directory / library)
                    cli(configuration, f"module load {library}")
                for rate, codec in ((0, ""), (16000, "slin"), (8000, "ulaw")):
                    audio_case(
                        configuration,
                        radio_configuration,
                        logfile,
                        process,
                        rate,
                        codec,
                    )
                cli(configuration, "module unload chan_rpt_fixture.so")
                assert "0 modules loaded" in cli(
                    configuration, "module show like chan_rpt_fixture"
                )
            except BaseException:
                print(logfile.read_text(encoding="utf-8", errors="replace"))
                raise
            finally:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=30)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=30)
        print("isolated Asterisk load/reload/unload integration passed")


if __name__ == "__main__":
    main()
