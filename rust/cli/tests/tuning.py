"""Exercise one CLI session with mTLS connection ownership and conditional lease updates."""

import json
import ssl
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

PORT = "e356f44c-1828-41a6-b730-0effeec0969c"
BOOT = "810587fd-efb6-4d0f-ac68-c53d137466e0"
REV = "923a5a6f-e912-4047-8ca8-8dd37ad2a00a"


def run(binary):
    """The client must keep its connection while stdin is idle, then submit one owned action."""
    events = []
    renewed = threading.Event()
    owner = None
    lease = ""
    version = ""

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def send_json(self, status, value, *, validator=True):
            body = json.dumps(value).encode()
            self.send_response(status)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Content-Type", "application/hal+json")
            if validator:
                self.send_header("ETag", version or '"rev"')
            self.end_headers()
            self.wfile.write(body)

        def dispatch(self):
            nonlocal owner, lease, version
            payload = json.loads(
                self.rfile.read(int(self.headers.get("Content-Length", "0"))) or b"{}"
            )
            method, path = self.command, self.path
            if lease and self.connection is not owner:
                self.send_json(403, {})
                return
            if method == "GET":
                docs = {
                    "/api/": {
                        "_links": {
                            "rpt:status": {"href": "/state/"},
                            "rpt:operations": {"href": "/ops/"},
                        }
                    },
                    "/port/": {
                        "id": PORT,
                        "_links": {"rpt:maintenance": {"href": "/tune/"}},
                    },
                    "/state/": {
                        "status": {"active_revision": REV},
                        "_links": {"rpt:active": {"href": "/active/"}},
                    },
                    "/active/": {"active_revision": REV},
                    "/tune/": {
                        "_links": {
                            "rpt:lease": {
                                "href": f"/tune/{BOOT}/{{lease}}/",
                                "templated": True,
                            }
                        }
                    },
                    "/ops/": {
                        "_links": {
                            "rpt:request": {
                                "href": f"/op/{BOOT}/{{request}}/",
                                "templated": True,
                            }
                        }
                    },
                    lease: {
                        "configuration": {
                            "active": True,
                            "expected_active_revision": REV,
                            "ttl_ms": 30000,
                        }
                    },
                }
                self.send_json(200 if path in docs else 404, docs.get(path, {}))
                return
            if (
                path.startswith("/tune/")
                and method == "PUT"
                and self.headers.get("If-None-Match") == "*"
            ):
                assert not lease
                assert payload == {
                    "configuration": {
                        "active": True,
                        "expected_active_revision": REV,
                        "ttl_ms": 30000,
                    }
                }
                owner, lease, version = self.connection, path, '"lease1"'
                events.append("acquire")
            elif path == lease and method == "PUT":
                assert self.headers.get("If-Match") == version
                assert payload["configuration"] == {
                    "active": True,
                    "expected_active_revision": REV,
                    "ttl_ms": 30000,
                }
                version = '"lease2"'
                events.append("renew")
                self.send_json(200, {}, validator=False)
                renewed.set()
                return
            elif path.startswith(f"/op/{BOOT}/") and method == "PUT":
                c = payload["configuration"]
                assert c == {
                    "action": {
                        "action": "radio_maintenance",
                        "operation": "get_mixer",
                        "target": "receive",
                    },
                    "targets": [PORT],
                    "expected_active_revision": REV,
                    "submitted": True,
                    "maintenance_lease": lease,
                }
                assert self.headers["If-None-Match"] == "*"
                events.append("operation")
                self.send_json(200, {"receipt": {"state": "completed", "outcomes": []}})
                return
            elif path == lease and method == "DELETE":
                assert self.headers["If-Match"] == version
                events.append("release")
            else:
                self.send_json(400, {})
                return
            self.send_json(200, {})

        do_GET = do_PUT = do_DELETE = dispatch

        def log_message(self, _format, *_args):
            """Keep request contents private."""

    pki = Path(__file__).resolve().parents[2] / "peer-support/tests/https-pki.sh"
    with tempfile.TemporaryDirectory() as name:
        tmp = Path(name)
        subprocess.run(["sh", str(pki), name], check=True, capture_output=True)
        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.load_cert_chain(tmp / "server.pem", tmp / "server.key")
        tls.load_verify_locations(tmp / "client.pem")
        tls.verify_mode = ssl.CERT_REQUIRED
        with HTTPServer(("127.0.0.1", 0), Handler) as server:
            server.socket = tls.wrap_socket(server.socket, server_side=True)
            thread = threading.Thread(target=server.serve_forever)
            thread.start()
            try:
                argv = [
                    binary,
                    "--url",
                    f"https://localhost:{server.server_port}/api/",
                    "--ca",
                    str(tmp / "backend.pem"),
                    "--cert",
                    str(tmp / "cli.pem"),
                    "--key",
                    str(tmp / "cli.key"),
                    "tune",
                    "/port/",
                ]
                with subprocess.Popen(
                    argv,
                    stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                ) as child:
                    try:
                        assert renewed.wait(9), (
                            "idle tuning input must retain the same connection and renew its lease"
                        )
                        stdout, stderr = child.communicate(
                            b'{"operation":"get_mixer","target":"receive"}\nquit\n',
                            timeout=10,
                        )
                        assert child.returncode == 0, (stdout, stderr)
                    finally:
                        if child.poll() is None:
                            child.kill()
                            child.wait()
                assert events == ["acquire", "renew", "operation", "release"], events
                owner, lease, version = None, "", ""
                events.clear()
                duplicate = subprocess.run(
                    argv,
                    input=b'{"operation":"transmit","operation":"get_mixer","target":"receive"}\n',
                    capture_output=True,
                    timeout=10,
                    check=False,
                )
                assert duplicate.returncode != 0
                assert events == ["acquire", "release"], events
            finally:
                server.shutdown()
                thread.join()


if __name__ == "__main__":
    run(sys.argv[1])
