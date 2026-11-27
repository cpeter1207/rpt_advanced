"""Read one status message over an authenticated fixture socket."""

import base64
import json
import os
import socket
import ssl
import sys
from pathlib import Path


def first_status_page(keys: Path, port: int, target: str) -> dict:
    tls = ssl.create_default_context(cafile=keys / "backend.pem")
    tls.load_cert_chain(keys / "cli.pem", keys / "cli.key")
    with (
        socket.create_connection(("127.0.0.1", port), timeout=5) as raw,
        tls.wrap_socket(raw, server_hostname="localhost") as client,
    ):
        nonce = base64.b64encode(os.urandom(16)).decode("ascii")
        client.sendall(
            (
                f"GET {target} HTTP/1.1\r\n"
                f"Host: localhost:{port}\r\n"
                "Connection: Upgrade\r\n"
                "Upgrade: websocket\r\n"
                "Sec-WebSocket-Version: 13\r\n"
                f"Sec-WebSocket-Key: {nonce}\r\n"
                "Sec-WebSocket-Protocol: rptadv.status.v1\r\n\r\n"
            ).encode("ascii")
        )
        with client.makefile("rb") as source:
            status = source.readline(4096)
            assert status.startswith(b"HTTP/1.1 101 "), status
            for _ in range(32):
                if source.readline(4096) == b"\r\n":
                    break
            first, length = source.read(2)
            assert first == 0x81 and length < 128
            if length in (126, 127):
                length = int.from_bytes(source.read(2 if length == 126 else 8), "big")
            assert 0 < length <= 4 * 1024 * 1024
            return json.loads(source.read(length))


if __name__ == "__main__":
    print(
        json.dumps(first_status_page(Path(sys.argv[1]), int(sys.argv[2]), sys.argv[3]))
    )
