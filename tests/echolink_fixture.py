"""Loopback-only EchoLink Proxy and directory for installed-host tests."""

import queue
import socket
import struct
import threading


class EchoProxy:
    """Scripted account transport; no public network or account is contacted."""

    # Independent EchoLib legacy SDES fixture, N0CALL with an empty station name.
    SDES = bytes.fromhex(
        "c0c9000100000000c1ca000d00000000010843414c4c5349474e020f"
        "4e3043414c4c202020202020202020030843414c4c5349474e040530383a333000000000"
    )
    # Asterisk20.21.0 codecs/ex_gsm.h, Digium2008, GPL-2.0.
    GSM = bytes.fromhex(
        "daa6ac2da3500049249249245040492492372452004924924724508046e36db8dc"
    )

    def __init__(self):
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(1)
        self.listener.settimeout(5)
        self.port = self.listener.getsockname()[1]
        self.connection = None
        self.requests = queue.Queue()
        self.packets = queue.Queue()
        self.ready = threading.Event()
        self.stopping = threading.Event()
        self.writer = threading.Lock()
        self.error = None
        self.thread = threading.Thread(target=self._run, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, kind, _value, _traceback):
        self.stopping.set()
        if self.connection:
            try:
                self.connection.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            self.connection.close()
        self.listener.close()
        self.thread.join(6)
        assert not self.thread.is_alive(), "Echo fixture did not stop"
        if kind is None and self.error is not None:
            raise self.error

    def send(self, kind, payload, address=b"\x7f\0\0\x02"):
        """Submit one complete Proxy record; test writes cannot interleave."""
        assert self.ready.wait(5), "Echo fixture did not authenticate"
        packet = struct.pack("<B4sI", kind, address, len(payload)) + payload
        with self.writer:
            self.connection.sendall(packet)

    def voice(self, sequence):
        """Send one independent, four-frame GSM packet on its source clock."""
        self.send(5, struct.pack("!BBHII", 0xC0, 3, sequence, 0, 0) + self.GSM * 4)

    def _run(self):
        try:
            self.connection, _ = self.listener.accept()
            self.connection.settimeout(5)
            self.connection.sendall(b"12345678")
            auth = bytearray()
            while len(auth) < 24:
                part = self.connection.recv(24 - len(auth))
                if not part:
                    raise ConnectionError("EOF during fixture authentication")
                auth.extend(part)
            assert auth[:8] == b"K1ABC-R\n"
            self.ready.set()
            self.connection.settimeout(0.1)
            pending = bytearray()
            while not self.stopping.is_set():
                try:
                    part = self.connection.recv(4096)
                except TimeoutError:
                    continue
                if not part:
                    break
                pending.extend(part)
                while len(pending) >= 9:
                    kind, address, size = struct.unpack("<B4sI", pending[:9])
                    assert size <= 65536, "unbounded fixture record"
                    if len(pending) < size + 9:
                        break
                    payload = bytes(pending[9 : 9 + size])
                    del pending[: 9 + size]
                    assert address in (b"\x7f\0\0\x01", b"\x7f\0\0\x02")
                    if kind == 1:
                        self.send(4, bytes(4), bytes(4))
                    elif kind == 2:
                        self.requests.put(payload)
                        response = (
                            b"@@@\n1\nN0CALL\nFixture\n123456\n127.0.0.2\n+++\n"
                            if payload == b"F0\r"
                            else b"OK"
                        )
                        self.send(2, response, bytes(4))
                    elif kind == 3:
                        self.send(3, b"", bytes(4))
                    else:
                        self.packets.put((kind, payload))
        except (AssertionError, OSError, struct.error) as error:
            if not self.stopping.is_set():
                self.error = error
