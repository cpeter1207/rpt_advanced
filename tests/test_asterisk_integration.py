#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""! @brief Load, reload, and unload the built module in an isolated Asterisk process."""

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


def main() -> None:
    """! @brief Verify the real module ABI and always stop the test-owned process.
    @return None; failures raise an exception.
    """
    with tempfile.TemporaryDirectory(prefix="rpt-advanced-asterisk-") as temporary:
        directory = Path(temporary)
        configuration = directory / "asterisk.conf"
        configuration.write_text(
            "[directories]\n"
            f"astetcdir => {directory}\n"
            f"astmoddir => {Path('build/stage/usr/lib/asterisk/modules').resolve()}\n"
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
                    if "app_rpt_advanced.so" in listing and "Running" in listing:
                        break
                    if process.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError(f"module did not start: {listing}")
                    time.sleep(0.1)
                radio_configuration.write_text(
                    "[usb]\nfull_duplex=yes\n", encoding="utf-8"
                )
                cli(configuration, "module reload app_rpt_advanced.so")
                listing = cli(configuration, "module show like app_rpt_advanced")
                assert "Running" in listing, listing
                cli(configuration, "module unload app_rpt_advanced.so")
                listing = cli(configuration, "module show like app_rpt_advanced")
                assert "0 modules loaded" in listing, listing
                cli(configuration, "module load app_rpt_advanced.so")
                listing = cli(configuration, "module show like app_rpt_advanced")
                assert "Running" in listing and process.poll() is None, listing
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
