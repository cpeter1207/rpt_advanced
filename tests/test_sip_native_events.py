"""Verify native REGISTER callback timing without vendor patches or raw-message logs."""

import ctypes as C
import socket
import sys
import time
from collections import deque

import test_sip_registration as test


def run(path):
    library = C.CDLL(path)
    sink_type = C.CFUNCTYPE(
        None, C.c_void_p, C.c_uint32, C.c_uint32, C.c_uint32, C.c_void_p, C.c_size_t
    )
    library.sip_native_create.argtypes = [C.c_char_p, sink_type, C.c_void_p]
    library.sip_native_create.restype = C.c_void_p
    for name in ("step", "shutdown", "destroy"):
        getattr(library, "sip_native_" + name).argtypes = [C.c_void_p]
    library.sip_native_authenticate.argtypes = [C.c_void_p, C.c_char_p]
    library.sip_native_register.argtypes = [C.c_void_p] + [C.c_char_p] * 4
    library.sip_native_register.restype = C.c_int
    observed = deque(maxlen=64)
    pending = None
    registered = False
    stopped = False

    @sink_type
    def sink(_, kind, code, expiry, realm, length):
        nonlocal pending, registered, stopped
        observed.append((kind, code))
        if kind == 1 and code in (401, 407):
            assert realm and 0 < length <= 256
            pending = (
                b"Digest:"
                + C.string_at(realm, length)
                + b":synthetic:synthetic-test-secret"
            )
        if kind == 1 and 200 <= code < 300:
            registered = True
        if kind == 3 and code == 200:
            stopped = True

    class Peer(socket.socket):
        last_sender = None

        def recvfrom(self, length):
            packet, self.last_sender = super().recvfrom(length)
            return packet, self.last_sender

    class Api:
        def service(self, ctx, _):
            nonlocal pending
            library.sip_native_step(ctx)
            if pending:
                library.sip_native_authenticate(ctx, pending)
                pending = None
            return 0

    api = Api()
    with Peer(socket.AF_INET, socket.SOCK_DGRAM) as peer:
        peer.bind(("127.0.0.1", 0))
        peer.setblocking(False)
        ctx = library.sip_native_create(b"sip:127.0.0.1:*;transport=udp", sink, None)
        assert ctx
        deadline = time.monotonic() + 3
        while True:
            api.service(ctx, 7)
            result = library.sip_native_register(
                ctx,
                b"sip:registrar.invalid",
                b"sip:synthetic@registrar.invalid",
                f"sip:127.0.0.1:{peer.getsockname()[1]};transport=udp".encode(),
                b"60",
            )
            if result != 1:
                assert result == 0
                break
            assert time.monotonic() < deadline
            time.sleep(0.002)
        while not registered:
            test.fixture(api, peer, ctx)
            assert time.monotonic() < deadline
        library.sip_test_destroyed_handles.restype = C.c_size_t
        before = library.sip_test_destroyed_handles()
        port = peer.getsockname()[1]
        wire = (
            f"OPTIONS sip:synthetic@registrar.invalid SIP/2.0\r\n"
            f"Via: SIP/2.0/UDP 127.0.0.1:{port};branch=z9hG4bK-options;rport\r\n"
            "From: <sip:probe@fixture.invalid>;tag=probe\r\n"
            "To: <sip:synthetic@registrar.invalid>\r\n"
            "Call-ID: independent-options@fixture.invalid\r\n"
            "CSeq: 1 OPTIONS\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n"
        )
        peer.sendto(wire.encode(), peer.last_sender)
        deadline = time.monotonic() + 3
        while True:
            api.service(ctx, 7)
            try:
                response, _ = peer.recvfrom(8192)
            except BlockingIOError:
                assert time.monotonic() < deadline
                time.sleep(0.002)
                continue
            assert response.startswith(b"SIP/2.0 200 ")
            break
        for _ in range(10):
            api.service(ctx, 7)
        assert library.sip_test_destroyed_handles() == before + 1, (
            "out-of-dialog OPTIONS retained its application-owned handle"
        )
        print("PASS out-of-dialog OPTIONS releases its application-owned handle")
        observed.clear()
        deadline = time.monotonic() + 8
        while True:
            request = test.fixture(api, peer, ctx, "silent")
            if request and request["cseq"].endswith("REGISTER"):
                break
            assert time.monotonic() < deadline
        for _ in range(100):
            api.service(ctx, 7)
            time.sleep(0.002)
        before = list(observed)
        assert not any(kind == 1 for kind, _ in before), (
            "refresh unexpectedly reported a start event"
        )
        print(
            "Silent refresh transmitted; native callbacks before any response:", before
        )

        def reply(code, phrase, extra=()):
            packet = [f"SIP/2.0 {code} {phrase}"]
            packet.extend(
                f"{name}: {request[name]}"
                for name in ("via", "from", "to", "call-id", "cseq")
            )
            packet.extend(extra)
            packet.extend(("Content-Length: 0", "", ""))
            peer.sendto("\r\n".join(packet).encode(), peer.last_sender)

        reply(100, "Trying")
        for _ in range(100):
            api.service(ctx, 7)
            time.sleep(0.002)
        print("Callbacks after explicit peer100:", list(observed))
        reply(200, "OK", (f"Contact: {request['contact']};expires=4", "Expires: 4"))
        for _ in range(10):
            api.service(ctx, 7)
            time.sleep(0.002)
        library.sip_native_shutdown(ctx)
        deadline = time.monotonic() + 35
        while not stopped:
            test.fixture(api, peer, ctx)
            assert time.monotonic() < deadline
        library.sip_native_destroy(ctx)
        print("PASS native refresh observation probe cleanup")


if __name__ == "__main__":
    run(sys.argv[1])
