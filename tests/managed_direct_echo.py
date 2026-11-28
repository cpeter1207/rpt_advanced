"""Loopback directory and direct UDP peer for the managed foreground fixture."""

import ctypes
import os
import select
import socket
import sys
import time
from pathlib import Path

from echolink_fixture import EchoProxy  # Reuse the independent N0CALL SDES bytes.


def field(connection):
    data = bytearray()
    while len(data) <= 4096:
        part = connection.recv(1)
        if part == b"\r":
            return bytes(data)
        if not part:
            break
        data.extend(part)
    raise ValueError("incomplete directory field")


def handle_directory(listener, events, boot):
    stream, _ = listener.accept()
    with stream:
        (events / "contact").touch()
        stream.settimeout(2)
        first = field(stream)
        if first in (b"F0", b"Fmanaged"):
            assert (events / f"login-{boot}").exists(), "snapshot before registration"
            stream.sendall(b"@@@\n1:managed\nN0CALL\nFixture\n123456\n127.0.0.2\n+++\n")
            if first == b"Fmanaged":
                (events / f"fresh-{boot}").touch()
            return False
        if first != b"lK1ABC-R\xac\xacfixture-writeonly":
            raise ValueError("unexpected directory identity or credential")
        status = field(stream)
        field(stream)  # station description
        logout = status == b"OFF-V3.40"
        if not logout:
            assert status.startswith(b"ONLINE"), "unexpected registration status"
            field(stream)  # email
        stream.sendall(b"OK")
        (events / f"{'logout' if logout else 'login'}-{boot}").touch()
        return logout


def listen(events, port):
    # Independently guard standalone invocation of this peer before opening fixed ports.
    parent = os.environ["RPTADV_TEST_PARENT_NETNS"]
    if (
        not parent.startswith("net:[")
        or not parent.endswith("]")
        or os.readlink("/proc/self/ns/net") == parent
    ):
        raise RuntimeError("private network namespace required")
    interfaces = Path("/proc/net/dev").read_text().splitlines()[2:]
    if [line.split(":", 1)[0].strip() for line in interfaces] != ["lo"]:
        raise RuntimeError("loopback-only network namespace required")
    with (
        socket.socket() as directory,
        socket.socket(type=socket.SOCK_DGRAM) as audio,
        socket.socket(type=socket.SOCK_DGRAM) as control,
    ):
        directory.bind(("127.0.0.3", 5200))
        directory.listen()
        audio.bind(("127.0.0.2", 5198))
        control.bind(("127.0.0.2", 5199))
        (events / "ready").touch()
        deadline = time.monotonic() + 100
        for boot in (1, 2):
            connected = False
            pcm_seen = False
            while time.monotonic() < deadline:
                readable, _, _ = select.select([directory, control, audio], [], [], 0.2)
                if directory in readable and handle_directory(directory, events, boot):
                    assert pcm_seen, "foreground stopped before decoded station audio"
                    break
                if control in readable:
                    packet, source = control.recvfrom(2048)
                    if b"K1ABC-R" in packet and packet.startswith(b"\xc0\xc9"):
                        assert source == ("127.0.0.1", port + 1)
                        assert (events / f"fresh-{boot}").exists()
                        control.sendto(EchoProxy.SDES, source)
                        connected = True
                if audio in readable:
                    packet, source = audio.recvfrom(2048)
                    if (
                        connected
                        and not pcm_seen
                        and len(packet) == 144
                        and packet[0] == 0xC0
                        and packet[1] & 0x7F == 3
                    ):
                        assert source == ("127.0.0.1", port)
                        if decoded_signal(packet[12:]):
                            (events / f"pcm-{boot}").touch()
                            pcm_seen = True
            else:
                raise TimeoutError("direct foreground peer timed out")


def decoded_signal(payload):
    """Decode the four actual GSM frames at the receiving end."""
    gsm = ctypes.CDLL("libgsm.so.1")
    gsm.gsm_create.argtypes = []
    gsm.gsm_create.restype = ctypes.c_void_p
    gsm.gsm_decode.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.c_ubyte),
        ctypes.POINTER(ctypes.c_short),
    ]
    gsm.gsm_decode.restype = ctypes.c_int
    gsm.gsm_destroy.argtypes = [ctypes.c_void_p]
    gsm.gsm_destroy.restype = None
    decoder = gsm.gsm_create()
    assert decoder, "GSM receiver allocation failed"
    try:
        peak = 0
        for offset in range(0, 132, 33):
            frame = (ctypes.c_ubyte * 33).from_buffer_copy(payload, offset)
            samples = (ctypes.c_short * 160)()
            assert gsm.gsm_decode(decoder, frame, samples) == 0, "invalid GSM frame"
            peak = max(peak, max(map(abs, samples)))
        return peak > 256
    finally:
        gsm.gsm_destroy(decoder)


if __name__ == "__main__":
    listen(Path(sys.argv[1]), int(sys.argv[2]))
