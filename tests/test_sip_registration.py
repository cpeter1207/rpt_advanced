"""Independent synthetic SIP/UDP registrar for the registration adapter ABI."""

import ctypes as C
import hashlib
import os
import re
import socket
import sys
import time


class Span(C.Structure):
    _fields_ = [("data", C.c_char_p), ("length", C.c_size_t)]


class Config(C.Structure):
    _fields_ = [
        ("struct_size", C.c_uint32),
        ("expires", C.c_uint32),
        ("generation", C.c_uint64),
    ] + [
        (name, Span)
        for name in (
            "bind",
            "next_hop",
            "registrar",
            "aor",
            "username",
            "password",
        )
    ]


class Status(C.Structure):
    _fields_ = [
        (name, kind)
        for name, kind in (
            ("struct_size", C.c_uint32),
            ("state", C.c_uint32),
            ("generation", C.c_uint64),
            ("sequence", C.c_uint64),
            ("sip_status", C.c_uint32),
            ("accepted_expires", C.c_uint32),
            ("registrations", C.c_uint32),
            ("events_dropped", C.c_uint32),
        )
    ]


PTR = C.c_void_p
CREATE = C.CFUNCTYPE(C.c_int, C.POINTER(Config), C.POINTER(PTR))
SERVICE = C.CFUNCTYPE(C.c_int, PTR, C.c_uint64)
READ = C.CFUNCTYPE(C.c_int, PTR, C.c_uint64, C.POINTER(Status))
ACTION = C.CFUNCTYPE(C.c_int, PTR, C.c_uint64)


class Descriptor(C.Structure):
    _fields_ = [
        ("struct_size", C.c_uint32),
        ("abi_version", C.c_uint32),
        ("capability", C.c_char * 16),
        ("create", CREATE),
        ("service", SERVICE),
        ("next_event", READ),
        ("status", READ),
        ("register", ACTION),
        ("unregister", ACTION),
        ("shutdown", ACTION),
        ("destroy", ACTION),
    ]


def config(port):
    result = Config(C.sizeof(Config), 1, 7)
    # The nonresolvable SIP identity must never be confused with routing.
    values = (
        b"127.0.0.1:0",
        f"127.0.0.1:{port}".encode(),
        b"sip:registrar.invalid",
        b"sip:synthetic@registrar.invalid",
        b"synthetic",
        b"synthetic-test-secret",
    )
    for (name, _), value in zip(Config._fields_[3:], values, strict=True):
        setattr(result, name, Span(value, len(value)))
    return result


def digest_ok(header, method):
    # Match only the synthetic credential; never print an auth header on failure.
    fields = {
        key: quoted or token
        for key, quoted, token in re.findall(r'(\w+)=(?:"([^"]*)"|([^, ]+))', header)
    }

    def md5(text):
        return hashlib.md5(text.encode(), usedforsecurity=False).hexdigest()

    first = md5("synthetic:fixture:synthetic-test-secret")
    last = md5(f"{method}:{fields.get('uri', '')}")
    if fields.get("qop"):
        value = f"{first}:fixture-nonce:{fields['nc']}:{fields['cnonce']}:auth:{last}"
    else:
        value = f"{first}:fixture-nonce:{last}"
    return fields.get("response") == md5(value)


def fixture(api, peer, ctx, mode="success"):
    """Service one native turn then independently answer any loopback requests."""
    assert api.service(ctx, 7) == 0
    time.sleep(0.002)
    try:
        packet, address = peer.recvfrom(65536)
    except BlockingIOError:
        return None
    text = packet.decode("ascii")
    first, *lines = text.split("\r\n")
    headers = {}
    for line in lines:
        if not line:
            break
        if ":" in line:
            key, value = line.split(":", 1)
            name = key.strip().lower()
            name = {"v": "via", "f": "from", "t": "to", "i": "call-id"}.get(name, name)
            headers[name] = value.strip()
    if first.startswith("OPTIONS "):
        assert first == "OPTIONS sip:synthetic@registrar.invalid SIP/2.0"
        reply = ["SIP/2.0 200 OK"]
        reply.extend(
            f"{name}: {headers[name]}"
            for name in ("via", "from", "to", "call-id", "cseq")
        )
        reply.extend(("Content-Length: 0", "", ""))
        peer.sendto("\r\n".join(reply).encode(), address)
        return headers
    assert first == "REGISTER sip:registrar.invalid SIP/2.0", "SIP authority lost"
    assert "sip:synthetic@registrar.invalid" in headers["to"]
    auth = headers.get("authorization", "")
    valid = auth and digest_ok(auth, "REGISTER")
    if mode == "silent":
        return headers
    code, phrase = (
        (200, "OK")
        if valid and mode not in ("reject", "refresh-reject")
        else (401, "Unauthorized")
    )
    reply = [f"SIP/2.0 {code} {phrase}"]
    for name in ("via", "from", "to", "call-id", "cseq"):
        reply.append(f"{name}: {headers[name]}")
    if code == 401:
        realm = "refresh-fixture" if mode == "refresh-reject" else "fixture"
        reply.append(
            f'WWW-Authenticate: Digest realm="{realm}", nonce="fixture-nonce", algorithm=MD5, qop="auth"'
        )
    else:
        expiry = "0" if headers.get("expires") == "0" else "4"
        contact = re.sub(r";expires=\d+", "", headers["contact"])
        reply.append(f"Contact: {contact};expires={expiry}")
        reply.append(f"Expires: {expiry}")
    reply.extend(("Content-Length: 0", "", ""))
    peer.sendto("\r\n".join(reply).encode(), address)
    return headers


def snapshot(api, ctx):
    status = Status(struct_size=C.sizeof(Status))
    assert api.status(ctx, 7, C.byref(status)) == 0
    return status


def until(api, peer, ctx, predicate, mode="success", seconds=12):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        fixture(api, peer, ctx, mode)
        status = snapshot(api, ctx)
        if predicate(status):
            return status
    raise AssertionError(
        f"bounded progress failed: state={status.state}, code={status.sip_status}"
    )


def run(path):
    library = C.CDLL(path)
    symbol = library.rptadv_sip_adapter_descriptor_v1
    symbol.restype = C.POINTER(Descriptor)
    api = symbol().contents
    assert api.struct_size == C.sizeof(Descriptor)
    assert api.abi_version == 1 and api.capability == b"rptadv.sip"
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as peer:
        peer.bind(("127.0.0.1", 0))
        peer.setblocking(False)
        cfg = config(peer.getsockname()[1])
        ctx = PTR()
        assert api.create(C.byref(cfg), C.byref(ctx)) == 0, "account create unavailable"
        assert api.service(ctx, 8) == -2
        assert api.destroy(ctx, 7) == 1, "live context must retain native lease"
        assert snapshot(api, ctx).state == 0
        for _ in range(20):
            assert fixture(api, peer, ctx) is None, "prepare sent unauthorized REGISTER"
        assert api.register(ctx, 7) == 0
        assert api.register(ctx, 7) == -3
        status = until(api, peer, ctx, lambda s: s.registrations >= 2)
        assert status.state == 2 and status.accepted_expires == 4
        assert api.unregister(ctx, 7) == 0
        until(api, peer, ctx, lambda s: s.state == 4)
        assert api.shutdown(ctx, 7) == 0
        until(api, peer, ctx, lambda s: s.state == 7)
        assert api.destroy(ctx, 7) == 0
    print(
        "PASS independent REGISTER/Digest/accepted-expiry refresh/unregister/shutdown"
    )

    interrupted_refresh(api)
    failure_cases(api)


def failure_cases(api):
    invalid_inputs(api)
    for mode in ("reject", "silent"):
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as peer:
            peer.bind(("127.0.0.1", 0))
            peer.setblocking(False)
            cfg = config(peer.getsockname()[1])
            ctx = PTR()
            assert api.create(C.byref(cfg), C.byref(ctx)) == 0
            assert api.register(ctx, 7) == 0
            until(api, peer, ctx, lambda status: status.state == 5, mode, seconds=38)
            for _ in range(40):
                assert fixture(api, peer, ctx, mode) is None, (
                    "failed account kept sending"
                )
            assert api.shutdown(ctx, 7) == 0
            until(api, peer, ctx, lambda status: status.state == 7, mode)
            assert api.destroy(ctx, 7) == 0
        print(f"PASS bounded {mode} registration failure and cleanup")


def interrupted_refresh(api):
    """A challenged refresh cannot strand a queued removal in credential wait."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as peer:
        peer.bind(("127.0.0.1", 0))
        peer.setblocking(False)
        cfg = config(peer.getsockname()[1])
        ctx = PTR()
        assert api.create(C.byref(cfg), C.byref(ctx)) == 0
        assert api.register(ctx, 7) == 0
        until(api, peer, ctx, lambda status: status.state == 2)
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            request = fixture(api, peer, ctx, "refresh-reject")
            if request and request["cseq"].endswith("REGISTER"):
                break
        else:
            raise AssertionError("automatic refresh never reached registrar")
        # The peer's new-realm challenge is pending delivery to the adapter.
        assert api.unregister(ctx, 7) == 0
        status = until(
            api, peer, ctx, lambda status: status.state == 5, "refresh-reject"
        )
        assert status.sip_status == 401
        assert api.shutdown(ctx, 7) == 0
        until(api, peer, ctx, lambda status: status.state == 7, "refresh-reject")
        assert api.destroy(ctx, 7) == 0
    print("PASS exhausted refresh challenge terminates queued removal and cleans up")


def invalid_inputs(api):
    cfg = config(9)
    ctx = PTR(1)
    assert api.create(None, C.byref(ctx)) == -1 and not ctx
    assert api.create(C.byref(cfg), None) == -1
    for name, value in (
        ("struct_size", 4),
        ("generation", 0),
        ("expires", 0),
        ("expires", 65536),
    ):
        candidate = config(9)
        setattr(candidate, name, value)
        assert api.create(C.byref(candidate), C.byref(ctx)) == -1 and not ctx
    for name, value in (
        ("bind", b"hostname.invalid:5060"),
        ("next_hop", b"127.0.0.1:0"),
        ("next_hop", b"224.0.0.1:5060"),
        ("registrar", b"sip:registrar.invalid?Subject=unwanted"),
        ("aor", b"sip:synthetic@registrar.invalid\r\nVia: injected"),
        ("password", b"invalid\0secret"),
    ):
        candidate = config(9)
        setattr(candidate, name, Span(value, len(value)))
        assert api.create(C.byref(candidate), C.byref(ctx)) == -1 and not ctx
    os.environ["TPORT_LOG"] = "1"
    try:
        assert api.create(C.byref(cfg), C.byref(ctx)) == -4 and not ctx
    finally:
        del os.environ["TPORT_LOG"]
    assert api.create(C.byref(cfg), C.byref(ctx)) == 0
    for operation in (
        api.register,
        api.unregister,
        api.service,
        api.shutdown,
        api.destroy,
    ):
        assert operation(ctx, 8) == -2
    small = Status(struct_size=4)
    assert api.status(ctx, 7, C.byref(small)) == -1
    assert api.next_event(ctx, 7, C.byref(small)) == -1
    assert api.unregister(ctx, 7) == -3
    assert api.shutdown(ctx, 7) == 0
    for _ in range(1000):
        assert api.service(ctx, 7) == 0
        if snapshot(api, ctx).state == 7:
            break
        time.sleep(0.002)
    assert snapshot(api, ctx).state == 7
    assert api.destroy(ctx, 7) == 0
    print("PASS input refusal, preactivation cleanup and generation fences")


if __name__ == "__main__":
    run(sys.argv[1])
