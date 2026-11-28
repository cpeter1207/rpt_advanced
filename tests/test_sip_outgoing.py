"""Literal-wire outgoing SIP call; the real adapter owns signaling and auth."""

import ctypes as C
import socket
import sys
import time

import test_sip_calls as incoming
import test_sip_registration as registration

ORIGINATE = C.CFUNCTYPE(
    C.c_int,
    C.c_void_p,
    C.c_uint64,
    registration.Span,
    registration.Span,
    C.POINTER(C.c_uint64),
)


class Descriptor(C.Structure):
    _fields_ = incoming.Descriptor._fields_ + [
        ("originate_call", ORIGINATE),
        ("acknowledge_call", incoming.END),
        (
            "first_provisional",
            C.CFUNCTYPE(
                C.c_int, C.c_void_p, C.c_uint64, C.c_uint64, C.POINTER(incoming.Call)
            ),
        ),
    ]


def response(fixture, request, code, body=b"", challenge=False, nonce="fixture-nonce"):
    """Echo transaction headers but construct response semantics independently."""
    _, values, _ = request
    port = fixture.peer.getsockname()[1]
    reply = [f"SIP/2.0 {code} Fixture"]
    for name in ("via", "from", "to", "call-id", "cseq"):
        value = values[name]
        if name == "to" and ";tag=" not in value:
            value += ";tag=outgoing-recipient"
        reply.append(f"{name}: {value}")
    if challenge:
        reply.append(
            f'Proxy-Authenticate: Digest realm="fixture", nonce="{nonce}", algorithm=MD5, qop="auth"'
        )
    if body:
        reply += [
            f"Contact: <sip:recipient@127.0.0.1:{port}>",
            "Content-Type: application/sdp",
        ]
    reply += [f"Content-Length: {len(body)}", "", ""]
    fixture.peer.sendto("\r\n".join(reply).encode() + body, fixture.address)


def request(fixture, method, authenticated=False):
    return next(
        (
            item
            for item in reversed(fixture.responses)
            if item[0].startswith(method + " ")
            and (not authenticated or "proxy-authorization" in item[1])
        ),
        None,
    )


def happy_call(api):
    """Missing origination/auth/explicit-ACK or wrong response ownership fails."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as peer:
        peer.bind(("127.0.0.1", 0))
        peer.setblocking(False)
        f = incoming.Fixture(api, peer)
        try:
            token = C.c_uint64()
            target = b"sip:18005550199@registrar.invalid"
            code = api.originate_call(
                f.ctx,
                7,
                registration.Span(target, len(target)),
                registration.Span(incoming.OFFER, len(incoming.OFFER)),
                C.byref(token),
            )
            assert code == 0 and token.value, "outgoing call was not admitted"
            assert api.release_call(f.ctx, 7, token.value) == 1
            assert api.acknowledge_call(f.ctx, 7, token.value) == -3
            f.until(lambda: request(f, "INVITE") is not None)
            invite = request(f, "INVITE")
            assert invite[0] == "INVITE sip:18005550199@registrar.invalid SIP/2.0"
            assert "contact" in invite[1], "outgoing dialog has no Contact target"
            assert invite[2].split(b"\r\n\r\n", 1)[1] == incoming.OFFER
            response(f, invite, 407, challenge=True)
            f.until(lambda: request(f, "INVITE", authenticated=True) is not None)
            invite = request(f, "INVITE", authenticated=True)
            assert registration.digest_ok(invite[1]["proxy-authorization"], "INVITE")
            assert registration.snapshot(api, f.ctx).state == 2
            response(f, invite, 182)
            f.until(
                lambda: (
                    token.value in f.calls and f.calls[token.value].sip_status == 182
                )
            )
            assert f.calls[token.value].alerting_seen == 0
            response(f, invite, 180)
            f.until(
                lambda: (
                    token.value in f.calls and f.calls[token.value].sip_status == 180
                )
            )
            ringing = f.calls[token.value]
            assert ringing.state == 1 and ringing.offer_length == 0
            assert ringing.alerting_seen == 1 and ringing.invite_failure_status == 0
            # One INVITE/offer and one early dialog: all responses carry the same answer.
            descriptions = {}
            for code in (182, 183):
                response(f, invite, code, incoming.ANSWER)
                f.until(lambda code=code: f.calls[token.value].sip_status == code)
                progress = f.calls[token.value]
                assert progress.state == 1 and progress.direction == 1
                assert (
                    progress.alerting_seen == 1 and progress.invite_failure_status == 0
                )
                assert api.acknowledge_call(f.ctx, 7, token.value) == -3
                descriptions[code] = (
                    bytes(progress.offer[: progress.offer_length]) == incoming.ANSWER,
                    progress.source_family == 4
                    and bytes(progress.source[:4]) == socket.inet_aton("127.0.0.1")
                    and progress.source_port == peer.getsockname()[1],
                )
            early_description, early_source = descriptions[183]
            response(f, invite, 200, incoming.ANSWER)
            f.until(lambda: token.value in f.calls and f.calls[token.value].state == 2)
            answer = f.calls[token.value]
            assert answer.direction == 1, "outgoing answer entered incoming admission"
            assert bytes(answer.offer[: answer.offer_length]) == incoming.ANSWER
            assert answer.source_family == 4
            assert answer.source_port == peer.getsockname()[1]
            assert bytes(answer.source[:4]) == b"\x7f\x00\x00\x01"
            number = invite[1]["cseq"].split()[0]
            response(f, invite, 200, incoming.OFFER)
            for _ in range(20):
                f.step()
                time.sleep(0.002)
            assert not any(
                first.startswith("ACK ") and values["cseq"] == number + " ACK"
                for first, values, _ in f.responses
            ), "answer was acknowledged before host validation"
            assert (
                bytes(f.calls[token.value].offer[: answer.offer_length])
                == incoming.ANSWER
            )
            assert api.acknowledge_call(f.ctx, 7, token.value) == 0
            f.until(
                lambda: (
                    f.calls[token.value].state == 3
                    and any(
                        first.startswith("ACK ") and values["cseq"] == number + " ACK"
                        for first, values, _ in f.responses
                    )
                )
            )
            assert api.acknowledge_call(f.ctx, 7, token.value) == 0
            previous_acks = sum(
                first.startswith("ACK ") and values["cseq"] == number + " ACK"
                for first, values, _ in f.responses
            )
            response(f, invite, 200, incoming.ANSWER)
            f.until(
                lambda: (
                    sum(
                        first.startswith("ACK ") and values["cseq"] == number + " ACK"
                        for first, values, _ in f.responses
                    )
                    > previous_acks
                )
            )
            assert len(f.calls) == 1 and f.calls[token.value].state == 3
            values = invite[1]
            contact = values["contact"].split("<", 1)[-1].split(">", 1)[0]
            bye = [
                f"BYE {contact} SIP/2.0",
                f"Via: SIP/2.0/UDP 127.0.0.1:{peer.getsockname()[1]};branch=z9hG4bK-recipient-bye;rport",
                "Max-Forwards: 70",
                "From: " + values["to"] + ";tag=outgoing-recipient",
                "To: " + values["from"],
                "Call-ID: " + values["call-id"],
                "CSeq: 77 BYE",
                "Content-Length: 0",
                "",
                "",
            ]
            peer.sendto("\r\n".join(bye).encode(), f.address)
            f.until(lambda: f.calls[token.value].state == 5)
            assert api.release_call(f.ctx, 7, token.value) == 0
            assert api.release_call(f.ctx, 7, token.value) == -2
            assert registration.snapshot(api, f.ctx).state == 2
        finally:
            f.close()
    print(
        "SIP_PROGRESS_CLOSED digest=true ringing=true pending_183=true "
        f"early_sdp={early_description} early_source={early_source} "
        "final_sdp=true ack=true bye=true released=true shutdown=true"
    )
    assert early_description and early_source, (
        "pending outgoing 183 must expose its own complete SDP and actual signaling source"
    )
    assert all(descriptions[182]), (
        "native 182 must also publish complete SDP and source"
    )
    print(
        "PASS literal outgoing INVITE/Digest/answer/host ACK/remote BYE without account failure"
    )


def originate(fixture):
    """Start one bounded call without teaching the fixture any dialog behavior."""
    token = C.c_uint64()
    target = b"sip:18005550199@registrar.invalid"
    assert (
        fixture.api.originate_call(
            fixture.ctx,
            7,
            registration.Span(target, len(target)),
            registration.Span(incoming.OFFER, len(incoming.OFFER)),
            C.byref(token),
        )
        == 0
    )
    fixture.until(lambda: request(fixture, "INVITE") is not None)
    return token.value, request(fixture, "INVITE")


def coalesced_answer_observations(api):
    """Do not read any call observations between the first early SDP and final SDP."""
    read_type = C.CFUNCTYPE(
        C.c_int, C.c_void_p, C.c_uint64, C.c_uint64, C.POINTER(incoming.Call)
    )

    # End the prefix at the known v2 ACK slot even after Descriptor grows a tail.
    fields = Descriptor._fields_
    end = (
        next(i for i, (name, _) in enumerate(fields) if name == "acknowledge_call") + 1
    )

    class ThroughAcknowledge(C.Structure):
        _fields_ = fields[:end]

    class WithFirstProvisional(ThroughAcknowledge):
        _fields_ = (("first_provisional", read_type),)

    assert C.sizeof(ThroughAcknowledge) == 136 and C.sizeof(WithFirstProvisional) == 144
    assert WithFirstProvisional.first_provisional.offset == 136

    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as peer:
        peer.bind(("127.0.0.1", 0))
        peer.setblocking(False)
        f = incoming.Fixture(api, peer)
        try:
            token, invite = originate(f)
            final = incoming.ANSWER.replace(b"40002", b"40004")
            assert final != incoming.ANSWER
            response(f, invite, 180)
            response(f, invite, 183, incoming.ANSWER)
            response(f, invite, 200, final)
            # Sofia's subsequent independent OPTIONS response is the wire barrier.
            f.send("OPTIONS", number=191)
            end = time.monotonic() + 5
            while not f.response(191, "OPTIONS", 200):
                assert time.monotonic() < end, "OPTIONS after both answers"
                f.step(collect_calls=False)
                time.sleep(0.002)
            f.step()
            latest = f.calls[token]
            assert (
                latest.state == 2
                and latest.direction == 1
                and latest.alerting_seen == 1
            )
            assert (
                latest.sip_status == 200
                and bytes(latest.offer[: latest.offer_length]) == final
            )
            assert (
                latest.source_family == 4
                and latest.source_port == peer.getsockname()[1]
            )
            assert bytes(latest.source[:4]) == socket.inet_aton("127.0.0.1")
            retained = False
            read_first = None
            if api.struct_size >= C.sizeof(WithFirstProvisional):
                full = C.cast(C.pointer(api), C.POINTER(WithFirstProvisional)).contents
                read_first = full.first_provisional
                early = incoming.Call(struct_size=C.sizeof(incoming.Call))
                assert read_first(f.ctx, 8, token, C.byref(early)) == -2
                status = read_first(f.ctx, 7, token, C.byref(early))
                retained = (
                    status == 0
                    and early.call == token
                    and early.generation == 7
                    and early.state == 1
                    and early.sip_status == 183
                    and early.direction == 1
                    and bytes(early.offer[: early.offer_length]) == incoming.ANSWER
                    and early.source_family == 4
                    and early.source_port == peer.getsockname()[1]
                    and bytes(early.source[:4]) == socket.inet_aton("127.0.0.1")
                )
            # This final answer changed the offered media endpoint. Decline it.
            # The host close on a pending final still must ACK the 2xx and send BYE.
            assert api.hangup_call(f.ctx, 7, token) == 0
            f.until(
                lambda: (
                    f.calls[token].state == 5
                    and request(f, "ACK")
                    and request(f, "BYE")
                )
            )
            for method in ("ACK", "BYE"):
                assert request(f, method)[1]["call-id"] == invite[1]["call-id"]
            assert api.release_call(f.ctx, 7, token) == 0
            assert api.release_call(f.ctx, 7, token) == -2
            if read_first is not None:
                assert read_first(f.ctx, 7, token, C.byref(early)) == -2
            assert registration.snapshot(api, f.ctx).state == 2
        finally:
            f.close()
    print(
        f"SIP_COALESCED_CLOSED barrier=true final=true first={retained} "
        "declined_ack=true bye=true released=true shutdown=true",
        flush=True,
    )
    assert retained, (
        "first provisional SDP and source must survive a coalesced final answer"
    )


def lifecycle(api):
    """Each terminal path must retain one token without ending registration."""
    for mode in (
        "reject",
        "cancel",
        "cancel-answer",
        "invalid-answer",
        "local-bye",
        "pre-ack-close",
    ):
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as peer:
            peer.bind(("127.0.0.1", 0))
            peer.setblocking(False)
            f = incoming.Fixture(api, peer)
            try:
                token, invite = originate(f)
                if mode == "reject":
                    response(f, invite, 486)
                elif mode in ("cancel", "cancel-answer"):
                    response(f, invite, 180)
                    f.until(lambda f=f, token=token: f.calls[token].sip_status == 180)
                    assert api.hangup_call(f.ctx, 7, token) == 0
                    assert api.hangup_call(f.ctx, 7, token) == 0
                    f.until(lambda f=f: request(f, "CANCEL") is not None)
                    response(f, request(f, "CANCEL"), 200)
                    response(
                        f,
                        invite,
                        487 if mode == "cancel" else 200,
                        b"" if mode == "cancel" else incoming.ANSWER,
                    )
                    if mode == "cancel-answer":
                        f.until(lambda f=f: request(f, "BYE") is not None)
                        assert request(f, "ACK") is not None
                else:
                    response(
                        f,
                        invite,
                        200,
                        b"" if mode == "invalid-answer" else incoming.ANSWER,
                    )
                    if mode != "invalid-answer":
                        f.until(lambda f=f, token=token: f.calls[token].state == 2)
                        assert (
                            api.respond_call(f.ctx, 7, token, 480, registration.Span())
                            == -3
                        )
                        if mode == "local-bye":
                            assert api.acknowledge_call(f.ctx, 7, token) == 0
                            f.until(lambda f=f, token=token: f.calls[token].state == 3)
                        assert api.hangup_call(f.ctx, 7, token) == 0
                    f.until(lambda f=f: request(f, "BYE") is not None)
                    assert request(f, "ACK") is not None
                f.until(lambda f=f, token=token: f.calls[token].state == 5)
                if mode == "reject":
                    assert f.calls[token].invite_failure_status == 486
                assert api.release_call(f.ctx, 8, token) == -2
                assert api.release_call(f.ctx, 7, token) == 0
                assert api.release_call(f.ctx, 7, token) == -2
                assert api.acknowledge_call(f.ctx, 7, token) == -2
                assert registration.snapshot(api, f.ctx).state == 2
            finally:
                f.close()
        print("PASS outgoing terminal ownership: " + mode)


def capacity_and_failure(api):
    """Concurrent attempts retain distinct terminal owners and independent call failures."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as peer:
        peer.bind(("127.0.0.1", 0))
        peer.setblocking(False)
        f = incoming.Fixture(api, peer)
        target = b"sip:18005550199@registrar.invalid"
        destination = registration.Span(target, len(target))
        offer = registration.Span(incoming.OFFER, len(incoming.OFFER))
        try:
            tokens = []
            for _ in range(9):
                token = C.c_uint64()
                assert (
                    api.originate_call(f.ctx, 7, destination, offer, C.byref(token))
                    == 0
                )
                tokens.append(token.value)
            rejected = C.c_uint64(99)
            assert (
                api.originate_call(f.ctx, 8, destination, offer, C.byref(rejected))
                == -2
            )
            assert rejected.value == 0
            f.until(
                lambda: (
                    len(
                        {
                            item[1]["call-id"]
                            for item in f.responses
                            if item[0].startswith("INVITE ")
                        }
                    )
                    == 9
                )
            )
            f.send("INVITE", number=90, body=incoming.OFFER)
            f.until(lambda: any(call.direction == 0 for call in f.calls.values()))
            (incoming_token,) = (
                call.call for call in f.calls.values() if call.direction == 0
            )
            assert (
                f.api.respond_call(f.ctx, 7, incoming_token, 486, registration.Span())
                == 0
            )
            f.until(lambda: f.response(90, "INVITE", 486))
            f.send("ACK", number=90, to=f.response(90, "INVITE", 486)[1]["to"])
            f.until(lambda: f.calls[incoming_token].state == 5)
            assert api.release_call(f.ctx, 7, incoming_token) == 0
            invites = {
                item[1]["call-id"]: item
                for item in f.responses
                if item[0].startswith("INVITE ")
            }
            for item in invites.values():
                response(f, item, 486)
            f.until(lambda: all(f.calls[token].state == 5 for token in tokens))
            # Terminal observations are still owned while a fresh attempt starts.
            f.responses.clear()
            token, invite = originate(f)
            assert token > max(tokens)
            for old in tokens:
                assert api.release_call(f.ctx, 7, old) == 0
                assert api.release_call(f.ctx, 7, old) == -2
            response(f, invite, 407, challenge=True)
            f.until(lambda: request(f, "INVITE", authenticated=True) is not None)
            response(f, request(f, "INVITE", authenticated=True), 403)
            f.until(lambda: f.calls[token].state == 5)
            assert api.release_call(f.ctx, 7, token) == 0
            assert registration.snapshot(api, f.ctx).state == 2
        finally:
            f.close()
    print(
        "PASS shared outgoing/incoming capacity, stale token and call-only authentication failure"
    )


def repeated_authentication(api):
    """Exhausted call challenges release only their call, never the registration."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as peer:
        peer.bind(("127.0.0.1", 0))
        peer.setblocking(False)
        f = incoming.Fixture(api, peer)
        try:
            token, invite = originate(f)
            for attempt in range(3):
                prior = invite[1]["cseq"]
                response(
                    f, invite, 407, challenge=True, nonce=f"fixture-challenge-{attempt}"
                )
                f.until(
                    lambda prior=prior: (
                        f.calls[token].state >= 4
                        or request(f, "INVITE")[1]["cseq"] != prior
                    )
                )
                if f.calls[token].state >= 4:
                    break
                invite = request(f, "INVITE")
            f.until(lambda: f.calls[token].state == 5)
            assert api.release_call(f.ctx, 7, token) == 0
            assert registration.snapshot(api, f.ctx).state == 2
            assert (
                len(
                    {
                        item[1]["cseq"]
                        for item in f.responses
                        if item[0].startswith("INVITE ")
                    }
                )
                <= 3
            )
        finally:
            f.close()
    print(
        "PASS bounded repeated outgoing authentication, exact terminal release and live registration"
    )


def run(path):
    library = C.CDLL(path)
    getter = library.rptadv_sip_adapter_descriptor_v2
    getter.restype = C.POINTER(Descriptor)
    api = getter().contents
    assert api.struct_size >= C.sizeof(Descriptor), (
        "outgoing SIP capability is unavailable"
    )
    happy_call(api)
    coalesced_answer_observations(api)
    lifecycle(api)
    capacity_and_failure(api)
    repeated_authentication(api)


if __name__ == "__main__":
    run(sys.argv[1])
