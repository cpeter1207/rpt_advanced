#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""! @brief Exchange real IAX audio between two isolated rpt_advanced processes."""

import contextlib
import os
import re
import socket
import subprocess
import tempfile
import time
from pathlib import Path

from test_asterisk_integration import cli


def port():
    """! @brief Select an unused loopback UDP port.
    @return Ephemeral port number.
    """
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as endpoint:
        endpoint.bind(("127.0.0.1", 0))
        return endpoint.getsockname()[1]


@contextlib.contextmanager
def server(
    directory, modules, node, peer, local_port, peer_port, codec, radio, sample_rate=0
):
    """! @brief Own an isolated IAX/radio server and always terminate it.
    @param directory Test-owned configuration directory.
    @param modules Staged installed module directory.
    @param node Local node identity.
    @param peer Remote node identity.
    @param local_port Local IAX UDP port.
    @param peer_port Remote IAX UDP port.
    @param codec Permitted IAX codec.
    @param radio Phased synthetic receiver name.
    @param sample_rate Requested local PCM rate; zero selects native rate.
    @return Context yielding CLI configuration and output log paths.
    """
    directory.mkdir()
    configuration = directory / "asterisk.conf"
    configuration.write_text(
        "[directories]\n"
        + "".join(
            f"{name} => {directory}\n"
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
    (directory / "modules.conf").write_text(
        "[modules]\nautoload=no\n"
        + "".join(
            f"load={name}.so\n"
            for name in (
                "res_crypto",
                "res_timing_timerfd",
                "codec_ulaw",
                "codec_resample",
                "func_channel",
                "pbx_config",
                "chan_iax2",
                "chan_rpt_fixture",
                "app_rpt_advanced",
            )
        ),
        encoding="utf-8",
    )
    (directory / "iax.conf").write_text(
        f"[general]\nbindport={local_port}\nbindaddr=127.0.0.1\ndisallow=all\nallow={codec}\n"
        "[radio]\ntype=user\ncontext=incoming\nrequirecalltoken=no\n"
        f"disallow=all\nallow={codec}\n",
        encoding="utf-8",
    )
    (directory / "extensions.conf").write_text(
        f"[incoming]\nexten => {node},1,RptAdvanced({node})\n", encoding="utf-8"
    )
    directory_file = directory / "nodes.conf"
    directory_file.write_text(
        f"[extnodes]\n{peer}=radio@127.0.0.1:{peer_port}/{peer},127.0.0.1\n",
        encoding="utf-8",
    )
    (directory / "rpt_advanced.conf").write_text(
        f"[{node}]\nradio_channel={radio}\nlink_directory_file={directory_file}\n"
        f"sample_rate_hz={sample_rate}\n",
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
            deadline = time.monotonic() + 30
            while True:
                if process.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError("network test Asterisk did not start")
                if (directory / "asterisk.ctl").exists():
                    try:
                        if "RadioPlusAdvanced" in cli(
                            configuration, "core show channels concise"
                        ):
                            break
                    except subprocess.CalledProcessError:
                        pass
                time.sleep(0.1)
            yield configuration, logfile
        except BaseException:
            print(logfile.read_text(encoding="utf-8", errors="replace"))
            raise
        finally:
            process.terminate()
            try:
                process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=30)


def same_server(modules):
    """! @brief Verify one Asterisk process can originate and admit its own IAX call.
    @param modules Staged module directory.
    @return None; assertions require both node endpoints and complete cleanup.
    """
    with tempfile.TemporaryDirectory(prefix="rpt-advanced-same-server-") as temporary:
        directory = Path(temporary) / "server"
        endpoint = port()
        with server(
            directory,
            modules,
            "524950",
            "508422",
            endpoint,
            endpoint,
            "ulaw",
            "network-a",
        ) as (configuration, _):
            (directory / "nodes.conf").write_text(
                "[extnodes]\n"
                + "".join(
                    f"{node}=radio@127.0.0.1:{endpoint}/{node},127.0.0.1\n"
                    for node in ("524950", "508422")
                ),
                encoding="utf-8",
            )
            (directory / "extensions.conf").write_text(
                "[incoming]\n"
                "exten => 524950,1,RptAdvanced(524950)\n"
                "exten => 508422,1,RptAdvanced(508422)\n",
                encoding="utf-8",
            )
            node_configuration = directory / "rpt_advanced.conf"
            node_configuration.write_text(
                node_configuration.read_text(encoding="utf-8")
                + f"[508422]\nradio_channel=network-b\nlink_directory_file={directory / 'nodes.conf'}\n",
                encoding="utf-8",
            )
            cli(configuration, "dialplan reload")
            cli(configuration, "module reload app_rpt_advanced.so")
            result = cli(configuration, "rpt_advanced link connect 524950 508422")
            assert "completed" in result, result
            channels = cli(configuration, "core show channels concise")
            assert channels.count("IAX2/") >= 2, channels
            result = cli(configuration, "rpt_advanced link disconnect 524950 508422")
            assert "completed" in result, result
            deadline = time.monotonic() + 5
            while "IAX2/" in cli(configuration, "core show channels concise"):
                assert time.monotonic() < deadline, "same-server peer was not released"
                time.sleep(0.05)
            print("same-server IAX admission and disconnect passed")


def dtmf_links(modules):
    """! @brief Decode real PCM tones to connect and disconnect an IAX peer.
    @param modules Staged module directory.
    @return None; both actions must arise from the synthetic receiver's tones.
    """
    for rate in (8000, 16000, 48000):
        with tempfile.TemporaryDirectory(prefix="rpt-advanced-dtmf-") as temporary:
            directory = Path(temporary)
            first_port, second_port = port(), port()
            with (
                server(
                    directory / "b",
                    modules,
                    "508422",
                    "524950",
                    second_port,
                    first_port,
                    "ulaw",
                    "network-b",
                ) as second,
                server(
                    directory / "a",
                    modules,
                    "524950",
                    "508422",
                    first_port,
                    second_port,
                    "ulaw",
                    "network-dtmf",
                    rate,
                ) as first,
            ):
                deadline = time.monotonic() + 15
                while "IAX2/" not in cli(first[0], "core show channels concise"):
                    assert time.monotonic() < deadline, (
                        "received DTMF did not connect the link"
                    )
                    time.sleep(0.1)
                assert "IAX2/" in cli(second[0], "core show channels concise")
                deadline = time.monotonic() + 15
                while "IAX2/" in cli(first[0], "core show channels concise"):
                    assert time.monotonic() < deadline, (
                        "DTMF timeout did not disconnect the link"
                    )
                    time.sleep(0.1)
                log = first[1].read_text(encoding="utf-8")
                assert log.count("node 524950 link command completed") == 2, log
                print(
                    f"received DTMF connect/hash and disconnect/timeout at {rate} Hz passed"
                )


def main():
    """! @brief Test narrowband and wideband IAX audio plus connected reload cleanup.
    @return None; assertions require bidirectional remote audio.
    """
    modules = Path(os.environ["RPT_TEST_MODULE_DIR"]).resolve()
    candidates = list(Path("/usr/lib").glob("*/asterisk/modules/chan_iax2.so"))
    candidates += list(Path("/usr/lib/asterisk/modules").glob("chan_iax2.so"))
    assert candidates, "ASL3 test image lacks IAX2"
    for library in candidates[0].parent.glob("*.so"):
        destination = modules / library.name
        if not destination.exists():
            destination.symlink_to(library)
    for codec in ("ulaw", "slin16"):
        with tempfile.TemporaryDirectory(prefix="rpt-advanced-iax-") as temporary:
            directory = Path(temporary)
            first_port, second_port = port(), port()
            with (
                server(
                    directory / "a",
                    modules,
                    "524950",
                    "508422",
                    first_port,
                    second_port,
                    codec,
                    "network-a",
                ) as first,
                server(
                    directory / "b",
                    modules,
                    "508422",
                    "524950",
                    second_port,
                    first_port,
                    codec,
                    "network-b",
                ) as second,
            ):
                receiver_config = second[0].parent / "rpt_advanced.conf"
                original = receiver_config.read_text(encoding="utf-8")
                receiver_config.write_text(
                    original + "link_allow_nodes=524950\nlink_deny_nodes=524950\n",
                    encoding="utf-8",
                )
                cli(second[0], "module reload app_rpt_advanced.so")
                rejected = cli(first[0], "rpt_advanced link connect 524950 508422")
                assert "failed" in rejected, rejected
                receiver_config.write_text(original, encoding="utf-8")
                cli(second[0], "module reload app_rpt_advanced.so")
                result = cli(first[0], "rpt_advanced link connect 524950 508422")
                assert "completed" in result, result
                time.sleep(3)
                for configuration, _ in (first, second):
                    channels = cli(configuration, "core show channels concise")
                    assert "IAX2/" in channels, channels
                result = cli(first[0], "rpt_advanced link disconnect 524950 508422")
                assert "completed" in result, result
                deadline = time.monotonic() + 5
                while any(
                    "IAX2/" in cli(configuration, "core show channels concise")
                    for configuration, _ in (first, second)
                ):
                    assert time.monotonic() < deadline, "remote hangup was not reaped"
                    time.sleep(0.05)
                result = cli(second[0], "rpt_advanced link connect 508422 524950")
                assert "completed" in result, result
                time.sleep(1)
                for configuration, _ in (first, second):
                    (configuration.parent / "rpt_advanced.conf").write_text(
                        "", encoding="utf-8"
                    )
                    cli(configuration, "module reload app_rpt_advanced.so")
                for configuration, logfile in (first, second):
                    recorded = re.findall(
                        r"rpt_fixture network remote=(\d+)", logfile.read_text()
                    )
                    assert recorded and int(recorded[-1]) > 0, logfile.read_text()
                    assert "IAX2/" not in cli(
                        configuration, "core show channels concise"
                    )
                print(f"bidirectional IAX {codec} audio and connected reload passed")
    same_server(modules)
    dtmf_links(modules)


if __name__ == "__main__":
    main()
