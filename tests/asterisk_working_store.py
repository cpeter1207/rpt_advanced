# SPDX-License-Identifier: GPL-2.0-only
"""Managed configuration of one isolated Asterisk integration process."""

import base64
import http.client
import json
import os
import socket
import ssl
import subprocess
import time
import uuid
from contextlib import contextmanager
from pathlib import Path

PORTS = "/api/v1/configuration/ports/"
STATUS_PROTOCOL = "rptadv.status.v1"
CHANNELS = (
    "full",
    "half",
    "media",
    "network-a",
    "network-b",
    "network-dtmf",
    "network-echo",
    "network-full",
    "network-replacement",
)


class StatusSubscriber:
    """Read the status server's bounded frames in the test-owned TLS session."""

    def __init__(self, socket, source):
        self.socket = socket
        self.source = source

    def read_exact(self, size):
        payload = self.source.read(size)
        assert len(payload) == size, (len(payload), size)
        return payload

    def frame(self):
        first, size = self.read_exact(2)
        assert first in (0x81, 0x88, 0x89, 0x8A) and size < 128, (first, size)
        if size in (126, 127):
            size = int.from_bytes(self.read_exact(2 if size == 126 else 8), "big")
        assert size <= (4 * 1024 * 1024 if first == 0x81 else 125), (first, size)
        return first & 0x0F, self.read_exact(size)

    def send_control(self, opcode, payload):
        mask = os.urandom(4)
        masked = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
        self.socket.sendall(bytes((0x80 | opcode, 0x80 | len(payload))) + mask + masked)

    def await_control(self, wanted):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            opcode, payload = self.frame()
            if opcode == wanted:
                return payload
            if opcode == 9:
                self.send_control(10, payload)
            else:
                assert opcode == 1, (wanted, opcode)
        raise AssertionError(f"status stream did not send control opcode {wanted}")

    def ping(self):
        payload = os.urandom(8)
        self.send_control(9, payload)
        assert self.await_control(10) == payload

    def assert_drained(self):
        payload = self.await_control(8)
        assert int.from_bytes(payload[:2], "big") == 1001, payload
        assert self.source.read(1) == b""


class WorkingStore:
    """The test owns the service document, keys, listener, store and operator identity."""

    def __init__(self, directory: Path):
        self.directory = directory
        credentials = directory / "credentials"
        pki = directory / "pki"
        store = directory / "store"
        for path in (credentials, pki, store):
            path.mkdir(mode=0o700)
        subprocess.run(
            ["sh", "rust/peer-support/tests/https-pki.sh", pki],
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )

        def credential(name, data):
            path = credentials / name
            path.write_bytes(data)
            path.chmod(0o400)

        credential("rpt-advanced.aead.service-key-1", os.urandom(32))
        for name, cert in (
            ("server-leaf", "server"),
            ("client-ca", "client"),
            ("operator-leaf", "cli"),
        ):
            der = subprocess.check_output(
                ["openssl", "x509", "-in", pki / (cert + ".pem"), "-outform", "DER"]
            )
            credential("rpt-advanced.tls." + name, der)
        credential(
            "rpt-advanced.tls.server-key",
            subprocess.check_output(
                [
                    "openssl",
                    "pkcs8",
                    "-topk8",
                    "-nocrypt",
                    "-in",
                    pki / "server.key",
                    "-outform",
                    "DER",
                ]
            ),
        )
        credentials.chmod(0o500)
        self.environment = dict(os.environ, CREDENTIALS_DIRECTORY=str(credentials))
        fake = Path("build/asterisk-fixture").resolve()
        self.environment["LD_LIBRARY_PATH"] = os.pathsep.join(
            [
                str(fake),
                *filter(None, os.environ.get("LD_LIBRARY_PATH", "").split(os.pathsep)),
            ]
        )
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            self.port = listener.getsockname()[1]
        document = json.loads(Path("examples/rpt_advanced.service.json").read_bytes())
        store_id = str(uuid.uuid4())
        document.update(
            store_uuid=store_id,
            store_path=str(store),
            listen=f"127.0.0.1:{self.port}",
            authorities=[f"localhost:{self.port}"],
        )
        service = directory / "rpt_advanced.service.json"
        service.write_text(json.dumps(document), encoding="utf-8")
        service.chmod(0o600)
        subprocess.run(
            ["build/rpt-advanced-store", "init", store, store_id, "service-key-1"],
            env=self.environment,
            check=True,
            stdout=subprocess.PIPE,
        )
        self.context = ssl.create_default_context(cafile=str(pki / "backend.pem"))
        self.context.load_cert_chain(pki / "cli.pem", pki / "cli.key")
        self.context.set_alpn_protocols(["http/1.1"])
        self.ports = {}
        self.ready_offset = 0

    def request(self, method, path, body=None, guard=None):
        connection = http.client.HTTPSConnection(
            "localhost", self.port, context=self.context, timeout=20
        )
        try:
            headers = {"Accept": "application/hal+json"}
            if body is not None:
                headers["Content-Type"] = "application/hal+json"
            if guard:
                headers.update(guard)
            connection.request(
                method,
                path,
                None if body is None else json.dumps(body).encode(),
                headers,
            )
            reply = connection.getresponse()
            data = reply.read(1048577)
            assert len(data) <= 1048576, (method, path, reply.status)
            return (
                reply.status,
                {k.lower(): v for k, v in reply.getheaders()},
                json.loads(data) if data else {},
            )
        finally:
            connection.close()

    @contextmanager
    def status_stream(self, port_id):
        nonce = base64.b64encode(os.urandom(16)).decode("ascii")
        with (
            socket.create_connection(("127.0.0.1", self.port), timeout=5) as raw,
            self.context.wrap_socket(raw, server_hostname="localhost") as client,
            client.makefile("rb") as source,
        ):
            client.sendall(
                (
                    f"GET /api/v1/status/stream/?port={port_id}&topics=radio HTTP/1.1\r\n"
                    f"Host: localhost:{self.port}\r\n"
                    "Connection: Upgrade\r\n"
                    "Upgrade: websocket\r\n"
                    "Sec-WebSocket-Version: 13\r\n"
                    f"Sec-WebSocket-Key: {nonce}\r\n"
                    f"Sec-WebSocket-Protocol: {STATUS_PROTOCOL}\r\n\r\n"
                ).encode("ascii")
            )
            response = source.readline(4096)
            assert response.startswith(b"HTTP/1.1 101 "), response
            headers = http.client.parse_headers(source)
            assert headers["Sec-WebSocket-Protocol"] == STATUS_PROTOCOL
            yield StatusSubscriber(client, source)

    def start(self):
        deadline = time.monotonic() + 25
        while True:
            try:
                code, headers, _ = self.request("GET", "/api/v1/status/desired/")
            except (ConnectionError, OSError):
                if time.monotonic() >= deadline:
                    raise
                time.sleep(0.05)
                continue
            assert code == 200 and headers["etag"]
            break
        health = self.request("GET", "/api/v1/health/")[2]["status"]
        if (
            health["active_revision"] != health["desired_revision"]
            or health["historical_activation"] != "confirmed"
        ):
            boot = self.request("GET", "/api/v1/operations/")[2]["boot_id"]
            code, _, _ = self.request(
                "PUT",
                f"/api/v1/operations/{boot}/{uuid.uuid4()}/",
                {
                    "configuration": {
                        "submitted": True,
                        "action": {"action": "apply_configuration"},
                        "desired_etag": headers["etag"],
                    }
                },
                {"If-None-Match": "*"},
            )
            # Boot may already own activation when the HTTP listener first becomes available.
            assert code in (200, 201, 202, 409), code
        self.confirmed()

    def identity(self, key):
        return self.ports[key].rstrip("/").rsplit("/", 1)[1]

    def confirmed(self, revision=None):
        deadline = time.monotonic() + 20
        while True:
            status, _, view = self.request("GET", "/api/v1/health/")
            assert status == 200, status
            health = view["status"]
            if (
                health["historical_activation"] == "confirmed"
                and health["desired_revision"] == health["active_revision"]
                and (revision is None or health["active_revision"] == revision)
            ):
                return health
            assert time.monotonic() < deadline, (
                health["historical_activation"],
                health["active_revision"],
                revision,
            )
            time.sleep(0.02)

    def conflict_context(self, path, response):
        # Only bounded, public state from the isolated fixture. Never dump the request.
        error = response.get("error")
        context = {"error": error if error in ("conflict", "unavailable") else "other"}
        try:
            code, _, view = self.request("GET", "/api/v1/health/")
            context["health_http"] = code
            if code == 200:
                health = view["status"]
                context["activation"] = health["historical_activation"]
                context["desired_is_active"] = (
                    health["desired_revision"] == health["active_revision"]
                )
                context["active_is_last_good"] = (
                    health["active_revision"] == health["last_known_good"]
                )
                context["owner_count"] = len(health["owners"])
                context["port_owned"] = (
                    path.rstrip("/").split("/")[-1] in health["owners"]
                )
            with (self.directory / "console.log").open("rb") as log:
                log.seek(self.ready_offset)
                context["fixture_ready"] = sum(
                    b"rpt_fixture ready " in line for line in log
                )
        except (
            OSError,
            ValueError,
            KeyError,
            TypeError,
            http.client.HTTPException,
        ) as exc:
            context["observation_failure"] = type(exc).__name__
        return context

    def edit(self, method, path, body=None, reject=False):
        status, headers, _ = self.request("GET", path)
        assert status == 200, (path, status)
        code, headers, response = self.request(
            method, path, body, {"If-Match": headers["etag"]}
        )
        if reject:
            if code == 202:
                deadline = time.monotonic() + 20
                while True:
                    health = self.request("GET", "/api/v1/health/")[2]["status"]
                    if health["desired_revision"] == response[
                        "desired_revision"
                    ] and health["historical_activation"] in (
                        "failed",
                        "recovery_required",
                        "rolled_back",
                    ):
                        assert health["active_revision"] != response["desired_revision"]
                        break
                    assert time.monotonic() < deadline, health["historical_activation"]
                    time.sleep(0.02)
            else:
                assert code in (409, 503), (method, path, code)
        else:
            assert code in (200, 201, 202, 204), (
                method,
                path,
                code,
                self.conflict_context(path, response) if code == 409 else None,
            )
            self.confirmed(response.get("desired_revision"))
        return headers.get("location")

    def station(self, key, channel, duplex="full"):
        location = self.edit(
            "POST",
            PORTS,
            {
                "key": key,
                "configuration": {
                    "name": key,
                    "inherit_from": "site",
                    "enabled": False,
                },
            },
        )
        path = location.rstrip("/") + "/"
        self.ports[key] = path
        self.profile(
            key,
            "interface/",
            {"audio": {"device_identifier": f"3-{CHANNELS.index(channel) + 1}"}},
        )
        self.profile(key, "radio/", {"duplex_mode": duplex})
        return path

    def disabled_revision(self, key):
        health = self.confirmed()
        code, _, port = self.request("GET", self.ports[key])
        assert code == 200 and port["configuration"]["enabled"] is False
        revision = health["active_revision"]
        assert port["meta"]["configuration_revision"] == revision
        assert health["last_known_good"] == revision
        return revision

    def restore_disabled(self, key, prior):
        code, _, view = self.request("GET", "/api/v1/health/")
        assert code == 200
        health = view["status"]
        if health["desired_revision"] != health["active_revision"]:
            assert health["last_known_good"] == prior
            code, headers, desired = self.request("GET", "/api/v1/status/desired/")
            assert code == 200
            assert desired["desired_revision"] == health["desired_revision"]
            code, _, storage = self.request("GET", "/api/v1/status/storage/")
            assert code == 200 and storage["working_fingerprint"]
            code, _, operations = self.request("GET", "/api/v1/operations/")
            assert code == 200
            code, _, result = self.request(
                "PUT",
                f"/api/v1/operations/{operations['boot_id']}/{uuid.uuid4()}/",
                {
                    "configuration": {
                        "submitted": True,
                        "action": {"action": "restore_configuration"},
                        "restore": "last_known_good",
                        "desired_etag": headers["etag"],
                        "working_fingerprint": storage["working_fingerprint"],
                    }
                },
                {"If-None-Match": "*"},
            )
            assert code == 200 and result["receipt"]["state"] == "completed", code
        self.disabled_revision(key)

    def profile(self, key, resource, fields):
        self.edit("PUT", self.ports[key] + resource, {"configuration": fields})

    def enabled(self, key, value=True, reject=False):
        self.edit(
            "PUT",
            self.ports[key],
            {"configuration": {"name": key, "inherit_from": "site", "enabled": value}},
            reject,
        )

    def telemetry(self, key, kind, name, fields):
        return self.edit(
            "POST",
            self.ports[key] + f"telemetry/{kind}/",
            {"key": name, "configuration": fields},
        )

    def remove(self, key):
        self.edit("DELETE", self.ports[key])
        del self.ports[key]

    def asl(self, key, port, other, remote_port, denied=False):
        values = {
            "node_number": int(key),
            "iax": {"registration_url": "", "local_port": port},
            "lookup_method": "file",
            "directory_file": "",
            "static_nodes": {
                other: {"host": "127.0.0.1", "source": "127.0.0.1", "port": remote_port}
            },
        }
        if denied:
            values.update(allow_nodes=[other], deny_nodes=[other])
        return self.edit(
            "POST",
            self.ports[key] + "protocols/allstarlink/",
            {"configuration": values},
        )
