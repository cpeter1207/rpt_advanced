"""Literal-wire incoming call fixture; no production SIP parser builds requests."""

import ctypes as C
import socket
import sys
import time

import test_sip_registration as registration


class Call(C.Structure):
    _fields_ = [
        ("struct_size", C.c_uint32),
        ("state", C.c_uint32),
        ("generation", C.c_uint64),
        ("call", C.c_uint64),
        ("sip_status", C.c_uint32),
        ("offer_length", C.c_uint32),
        ("offer", C.c_ubyte * 4096),
        ("source_family", C.c_uint16),
        ("source_port", C.c_uint16),
        ("source", C.c_ubyte * 16),
    ]


NEXT = C.CFUNCTYPE(C.c_int, C.c_void_p, C.c_uint64, C.POINTER(Call))
RESPOND = C.CFUNCTYPE(
    C.c_int, C.c_void_p, C.c_uint64, C.c_uint64, C.c_uint32, registration.Span
)
END = C.CFUNCTYPE(C.c_int, C.c_void_p, C.c_uint64, C.c_uint64)


class Descriptor(C.Structure):
    _fields_ = registration.Descriptor._fields_ + [
        ("next_call", NEXT),
        ("respond_call", RESPOND),
        ("hangup_call", END),
        ("release_call", END),
    ]


OFFER = (
    b"v=0\r\no=fixture 1 1 IN IP4 127.0.0.1\r\ns=call\r\n"
    b"c=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 40000 RTP/AVP 0 101\r\n"
    b"a=rtpmap:0 PCMU/8000\r\na=rtpmap:101 telephone-event/8000\r\n"
    b"a=fmtp:101 0-15\r\na=ptime:20\r\na=sendrecv\r\n"
)
ANSWER = OFFER.replace(b"40000", b"40002").replace(b"fixture", b"adapter")


def headers(packet):
    first, *lines = packet.decode().split("\r\n\r\n", 1)[0].split("\r\n")
    result = {}
    for line in lines:
        key, value = line.split(":", 1)
        key = {"v": "via", "f": "from", "t": "to", "i": "call-id"}.get(
            key.lower(), key.lower()
        )
        result[key] = value.strip()
    return first, result


class Fixture:
    def __init__(self, api, peer):
        self.api, self.peer = api, peer
        self.ctx = C.c_void_p()
        cfg = registration.config(peer.getsockname()[1])
        cfg.expires = 600
        assert api.create(C.byref(cfg), C.byref(self.ctx)) == 0
        self.address = None
        self.responses, self.calls = [], {}
        self.hold_bye = False
        assert api.register(self.ctx, 7) == 0
        self.until(lambda: registration.snapshot(api, self.ctx).state == 2)

    def step(self):
        assert self.api.service(self.ctx, 7) == 0
        while True:
            call = Call(struct_size=C.sizeof(Call))
            code = self.api.next_call(self.ctx, 7, C.byref(call))
            if code == 1:
                break
            assert code == 0
            assert call.generation == 7 and call.call != 0
            self.calls[call.call] = call
        try:
            packet, address = self.peer.recvfrom(8192)
        except BlockingIOError:
            return
        first, values = headers(packet)
        if first.startswith(("REGISTER ", "OPTIONS ")):
            self.address = address
            reply = ["SIP/2.0 200 OK"]
            reply += [
                f"{name}: {values[name]}"
                for name in ("via", "from", "to", "call-id", "cseq")
            ]
            if "contact" in values:
                expiry = 0 if values.get("expires") == "0" else 600
                reply += [f"Contact: {values['contact']};expires={expiry}"]
            reply += ["Content-Length: 0", "", ""]
            self.peer.sendto("\r\n".join(reply).encode(), address)
        elif first.startswith("BYE "):
            if self.hold_bye:
                self.responses.append((first, values, packet))
                return
            reply = (
                ["SIP/2.0 200 OK"]
                + [
                    f"{name}: {values[name]}"
                    for name in ("via", "from", "to", "call-id", "cseq")
                ]
                + ["Content-Length: 0", "", ""]
            )
            self.peer.sendto("\r\n".join(reply).encode(), address)
            self.responses.append((first, values, packet))
        else:
            self.responses.append((first, values, packet))

    def until(self, predicate, seconds=5):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self.step()
            if predicate():
                return
            time.sleep(0.002)
        raise AssertionError(
            f"bounded call progress failed: account={registration.snapshot(self.api, self.ctx).state}, "
            f"calls={[(call.call, call.state) for call in self.calls.values()]}, "
            f"wire={[(first, values.get('cseq')) for first, values, _ in self.responses[-5:]]}"
        )

    def send(self, method, number=1, to=None, body=b""):
        port = self.peer.getsockname()[1]
        branch = f"z9hG4bK-fixture-{number}-" + (
            "invite" if method in ("INVITE", "CANCEL") else method.lower()
        )
        lines = [
            f"{method} sip:synthetic@registrar.invalid SIP/2.0",
            f"Via: SIP/2.0/UDP 127.0.0.1:{port};branch={branch};rport",
            "Max-Forwards: 70",
            f"From: <sip:caller@fixture.invalid>;tag=caller-{number}",
            "To: " + (to or "<sip:synthetic@registrar.invalid>"),
            f"Call-ID: independent-call-{number}@fixture.invalid",
            f"CSeq: {3 if method == 'BYE' else 2 if method == 'OPTIONS' else 1} {method}",
            f"Contact: <sip:caller@127.0.0.1:{port}>",
        ]
        if body:
            lines.append("Content-Type: application/sdp")
        lines += [f"Content-Length: {len(body)}", "", ""]
        self.peer.sendto("\r\n".join(lines).encode() + body, self.address)

    def response(self, number, method, code):
        return next(
            (
                response
                for response in self.responses
                if response[0].startswith(f"SIP/2.0 {code} ")
                and response[1]["call-id"]
                == f"independent-call-{number}@fixture.invalid"
                and response[1]["cseq"].endswith(method)
            ),
            None,
        )

    def close(self):
        assert self.api.unregister(self.ctx, 7) == 0
        self.until(lambda: registration.snapshot(self.api, self.ctx).state == 4)
        assert self.api.shutdown(self.ctx, 7) == 0
        self.until(lambda: registration.snapshot(self.api, self.ctx).state == 7, 35)
        assert self.api.destroy(self.ctx, 7) == 0


def lifecycle(api):
    """One fixture covers distinct remote/local retirement paths."""
    for mode in ("cancel", "reject", "local-bye", "shutdown"):
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as peer:
            peer.bind(("127.0.0.1", 0))
            peer.setblocking(False)
            f = Fixture(api, peer)
            f.send("INVITE", body=OFFER)
            f.until(lambda f=f: bool(f.calls))
            token = next(iter(f.calls))
            assert api.release_call(f.ctx, 7, token) == 1
            # An identical retransmission must not create another application owner.
            f.send("INVITE", body=OFFER)
            for _ in range(20):
                f.step()
                time.sleep(0.002)
            assert len(f.calls) == 1
            empty = registration.Span(None, 0)
            if mode == "cancel":
                f.send("CANCEL")
                f.until(lambda f=f: f.response(1, "INVITE", 487) is not None)
                f.send("ACK", to=f.response(1, "INVITE", 487)[1]["to"])
            elif mode == "reject":
                assert api.respond_call(f.ctx, 7, token, 488, empty) == 0
                f.until(lambda f=f: f.response(1, "INVITE", 488) is not None)
                f.send("ACK", to=f.response(1, "INVITE", 488)[1]["to"])
            elif mode == "local-bye":
                answer = registration.Span(ANSWER, len(ANSWER))
                assert api.respond_call(f.ctx, 7, token, 200, answer) == 0
                f.until(lambda f=f: f.response(1, "INVITE", 200) is not None)
                f.send("ACK", to=f.response(1, "INVITE", 200)[1]["to"])
                f.until(lambda f=f, token=token: f.calls[token].state == 3)
                assert api.hangup_call(f.ctx, 7, token) == 0
                assert api.hangup_call(f.ctx, 7, token) == 0
            else:
                assert api.shutdown(f.ctx, 7) == 0
            f.until(lambda f=f, token=token: f.calls[token].state == 5)
            assert api.release_call(f.ctx, 7, token) == 0
            assert api.release_call(f.ctx, 7, token) == -2
            if mode == "shutdown":
                f.until(lambda f=f: registration.snapshot(api, f.ctx).state == 7, 35)
                assert api.destroy(f.ctx, 7) == 0
            else:
                f.close()
        print(f"PASS duplicate INVITE and {mode} retain one terminal owner")


def delayed_ack(api):
    """A late ACK cannot revive an answered call already closing locally."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as peer:
        peer.bind(("127.0.0.1", 0))
        peer.setblocking(False)
        f = Fixture(api, peer)
        f.hold_bye = True
        f.send("INVITE", body=OFFER)
        f.until(lambda: bool(f.calls))
        token = next(iter(f.calls))
        answer = registration.Span(ANSWER, len(ANSWER))
        assert api.respond_call(f.ctx, 7, token, 200, answer) == 0
        f.until(lambda: f.response(1, "INVITE", 200) is not None)
        assert api.hangup_call(f.ctx, 7, token) == 0
        f.until(lambda: any(first.startswith("BYE ") for first, _, _ in f.responses))
        assert f.calls[token].state == 4
        f.send("ACK", to=f.response(1, "INVITE", 200)[1]["to"])
        for _ in range(20):
            f.step()
            assert f.calls[token].state == 4, "delayed ACK revived closing call"
            time.sleep(0.002)
        _, values, _ = next(r for r in f.responses if r[0].startswith("BYE "))
        reply = (
            ["SIP/2.0 200 OK"]
            + [
                f"{name}: {values[name]}"
                for name in ("via", "from", "to", "call-id", "cseq")
            ]
            + ["Content-Length: 0", "", ""]
        )
        peer.sendto("\r\n".join(reply).encode(), f.address)
        f.until(lambda: f.calls[token].state == 5)
        assert api.release_call(f.ctx, 7, token) == 0
        f.close()
    print("PASS delayed ACK cannot revive a closing call")


def capacity(api):
    """Eight slots remain owned until explicit release, independent of events."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as peer:
        peer.bind(("127.0.0.1", 0))
        peer.setblocking(False)
        f = Fixture(api, peer)
        for number in range(1, 9):
            f.send("INVITE", number=number, body=OFFER)
            f.until(lambda f=f, number=number: len(f.calls) == number)
        assert len({call.call for call in f.calls.values()}) == 8
        f.send("INVITE", number=9, body=OFFER)
        f.until(
            lambda: (
                f.response(9, "INVITE", 486) is not None
                or f.response(9, "INVITE", 503) is not None
            )
        )
        assert len(f.calls) == 8
        for number, token in enumerate(f.calls, 1):
            f.send("CANCEL", number=number)
            f.until(lambda f=f, token=token: f.calls[token].state == 5)
            f.until(
                lambda f=f, number=number: f.response(number, "INVITE", 487) is not None
            )
            f.send("ACK", number=number, to=f.response(number, "INVITE", 487)[1]["to"])
        # Terminal state is authoritative even though observations were consumed.
        f.send("INVITE", number=10, body=OFFER)
        f.until(lambda f=f: f.response(10, "INVITE", 486) is not None)
        for token in f.calls:
            assert api.release_call(f.ctx, 7, token) == 0
        old = max(f.calls)
        f.send("INVITE", number=11, body=OFFER)
        f.until(lambda f=f: max(f.calls) > old)
        token = max(f.calls)
        f.send("CANCEL", number=11)
        f.until(lambda f=f, token=token: f.calls[token].state == 5)
        f.until(lambda f=f: f.response(11, "INVITE", 487) is not None)
        f.send("ACK", number=11, to=f.response(11, "INVITE", 487)[1]["to"])
        assert api.release_call(f.ctx, 7, token) == 0
        f.close()
    print("PASS fixed capacity, retained terminal slots and nonreused call tokens")


def run(path):
    library = C.CDLL(path)
    symbol = library.rptadv_sip_adapter_descriptor_v1
    symbol.restype = C.POINTER(Descriptor)
    api = symbol().contents
    assert api.struct_size >= C.sizeof(Descriptor), "incoming call capability missing"
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as peer:
        peer.bind(("127.0.0.1", 0))
        peer.setblocking(False)
        f = Fixture(api, peer)
        f.send("INVITE", body=OFFER)
        f.until(lambda f=f: bool(f.calls))
        call_id, call = next(iter(f.calls.items()))
        assert call.state == 1 and bytes(call.offer[: call.offer_length]) == OFFER
        assert call.source_family == 4 and call.source_port == peer.getsockname()[1]
        assert bytes(call.source[:4]) == socket.inet_aton("127.0.0.1")
        assert f.response(1, "INVITE", 200) is None, "implicit answer"
        answer = registration.Span(ANSWER, len(ANSWER))
        assert api.respond_call(f.ctx, 8, call_id, 200, answer) == -2
        assert api.respond_call(f.ctx, 7, call_id + 1, 200, answer) == -2
        assert api.respond_call(f.ctx, 7, call_id, 200, answer) == 0
        f.until(lambda f=f: f.response(1, "INVITE", 200) is not None)
        response = f.response(1, "INVITE", 200)
        assert response[2].split(b"\r\n\r\n", 1)[1] == ANSWER
        f.send("ACK", to=response[1]["to"])
        f.until(lambda f=f: f.calls[call_id].state == 3)
        f.send("OPTIONS", to=response[1]["to"])
        f.until(lambda: f.response(1, "OPTIONS", 200) is not None)
        f.send("BYE", to=response[1]["to"])
        f.until(lambda f=f: f.calls[call_id].state == 5)
        f.until(lambda f=f: f.response(1, "BYE", 200) is not None)
        assert api.release_call(f.ctx, 7, call_id) == 0
        assert api.hangup_call(f.ctx, 7, call_id) == -2
        f.close()
    print("PASS literal REGISTER/INVITE/explicit SDP answer/ACK/BYE and stale tokens")
    lifecycle(api)
    delayed_ack(api)
    capacity(api)


if __name__ == "__main__":
    run(sys.argv[1])
