"""Verify outgoing methods, preconditions and operation identity on a real mTLS connection."""

import json
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

ROOT = "/fixture/"
BOOT = "d102044c-192b-4c29-b18c-2ab6e9d0e115"
ID = "ea2399e1-3249-4ab2-a6e6-d30e17fd91ae"
RECEIPT = f"/receipts/{BOOT}/{ID}/"


def run(binary):
    """Accept only the exact resource edit and operation contract below."""
    requests = []
    lost = RECEIPT.replace(ID, "e7a003c3-2473-4275-a70a-79ec4bee5d22")
    documents = {
        "/schema/": {"type": "object"},
        ROOT: {
            "_links": {
                "rpt:operations": {"href": "/ops/"},
                "rpt:status": {"href": "/state/"},
            }
        },
        "/ops/": {
            "_links": {
                "rpt:request": {
                    "href": f"/receipts/{BOOT}/{{request}}/",
                    "templated": True,
                }
            }
        },
        "/state/": {
            "_links": {
                "rpt:desired": {"href": "/desired/"},
                "rpt:storage": {"href": "/storage/"},
            },
            "status": {"active_revision": "rev1"},
        },
        "/desired/": {"desired_revision": "rev2"},
        "/storage/": {"working_fingerprint": "f" * 64},
        lost: {"receipt": {"state": "completed", "outcomes": []}},
        RECEIPT: {
            "receipt": {
                "state": "completed",
                "outcomes": [],
                "results": [{"target": "station", "mixer": {"receive": 125}}],
            }
        },
    }

    ambiguous = RECEIPT.replace(ID, "d109fb7c-343d-43e5-84cc-0b947f88d311")
    documents[ambiguous] = documents[lost]
    draft = RECEIPT.replace(ID, "8f8e6a2f-9adf-414e-a169-f0307a9d35ce")

    class Server(BaseHTTPRequestHandler):
        def dispatch(self):
            length = int(self.headers.get("Content-Length", "0"))
            body = self.rfile.read(length)
            requests.append((self.command, self.path, self.headers, body))
            if self.command == "PUT" and self.path == ambiguous:
                self.send_error(503)
                return
            if self.command == "PUT" and self.path == lost:
                self.connection.shutdown(socket.SHUT_RDWR)
                self.close_connection = True
                return
            if (
                self.path == "/schema/"
                and "application/schema+json" not in self.headers["Accept"]
            ):
                self.send_error(406)
                return
            if self.headers.get("If-Match") == '"stale"':
                self.send_error(412)
                return
            if self.command == "GET" and self.path not in documents:
                self.send_error(404)
                return
            reply = documents.get(self.path, {})
            if self.path == RECEIPT and self.command == "PUT":
                reply = {"receipt": {"state": "submitted"}}
            if self.path == draft and self.command == "PUT":
                assert json.loads(body) == {"configuration": {"submitted": False}}
                assert self.headers["If-None-Match"] == "*"
                reply = {"receipt": {"state": "draft"}}
            if self.path == draft and self.command == "PATCH":
                assert json.loads(body) == [
                    {"op": "replace", "path": "/configuration/submitted", "value": True}
                ]
                assert self.headers["If-Match"] == '"version"'
                reply = {"receipt": {"state": "completed"}}
            out = json.dumps(reply).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(out)))
            self.send_header("Content-Type", "application/hal+json")
            self.send_header("ETag", '"version"')
            self.end_headers()
            self.wfile.write(out)

        do_GET = do_POST = do_PATCH = do_PUT = do_DELETE = dispatch

        def log_message(self, _format, *_args):
            """Do not print request contents."""

    pki = Path(__file__).resolve().parents[2] / "peer-support/tests/https-pki.sh"
    with tempfile.TemporaryDirectory() as directory:
        tmp = Path(directory)
        subprocess.run(["sh", str(pki), directory], check=True, capture_output=True)
        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.load_cert_chain(tmp / "server.pem", tmp / "server.key")
        tls.load_verify_locations(tmp / "client.pem")
        tls.verify_mode = ssl.CERT_REQUIRED
        with HTTPServer(("127.0.0.1", 0), Server) as server:
            server.socket = tls.wrap_socket(server.socket, server_side=True)
            worker = threading.Thread(target=server.serve_forever)
            worker.start()
            try:
                cli = [
                    binary,
                    "--url",
                    f"https://localhost:{server.server_port}{ROOT}",
                    "--ca",
                    str(tmp / "backend.pem"),
                    "--cert",
                    str(tmp / "cli.pem"),
                    "--key",
                    str(tmp / "cli.key"),
                ]
                for method in ["PUT", "PATCH", "POST", "DELETE"]:
                    payload = (
                        b'[{"op":"remove","path":"/configuration/old"}]'
                        if method == "PATCH"
                        else b'{"configuration":{"name":"new"}}'
                    )
                    args = [method.lower(), "/item/", "--if-match", '"version"']
                    if method != "DELETE":
                        args += ["-"]
                    result = subprocess.run(
                        cli + args,
                        input=payload,
                        capture_output=True,
                        timeout=5,
                        check=False,
                    )
                    assert result.returncode == 0, (method, result.stderr)
                    verb, path, headers, content = requests[-1]
                    assert (verb, path, headers["If-Match"]) == (
                        method,
                        "/item/",
                        '"version"',
                    )
                    assert content == (b"" if method == "DELETE" else payload)
                    if method != "DELETE":
                        expected = (
                            "application/json-patch+json"
                            if method == "PATCH"
                            else "application/hal+json"
                        )
                        assert headers["Content-Type"] == expected, (
                            "controller requires exact mutation media type"
                        )
                before = len(requests)
                result = subprocess.run(
                    cli + ["delete", "/item/", "--if-match", '"stale"'],
                    capture_output=True,
                    check=False,
                )
                assert (
                    result.returncode
                    and b"412" in result.stderr
                    and len(requests) == before + 1
                )
                start = len(requests)
                result = subprocess.run(
                    cli + ["reload", "--request", ID],
                    capture_output=True,
                    timeout=8,
                    check=False,
                )
                assert result.returncode == 0, result.stderr
                assert json.loads(result.stdout)["receipt"]["state"] == "completed"
                assert json.loads(result.stdout) == documents[RECEIPT], (
                    "typed query results must remain intact"
                )
                changes = [r for r in requests[start:] if r[0] != "GET"]
                assert len(changes) == 1
                method, path, h, b = changes[0]
                assert (method, path, h["If-None-Match"]) == ("PUT", RECEIPT, "*")
                assert json.loads(b) == {
                    "configuration": {
                        "submitted": True,
                        "action": {"action": "reload_configuration"},
                        "desired_etag": '"version"',
                        "working_fingerprint": "f" * 64,
                    }
                }
                assert requests[-1][:2] == ("GET", RECEIPT)
                start = len(requests)
                lost_id = lost.split("/")[-2]
                result = subprocess.run(
                    cli + ["apply", "--request", lost_id],
                    capture_output=True,
                    timeout=8,
                    check=False,
                )
                assert result.returncode == 0, (
                    "a lost PUT reply must reconcile the exact receipt",
                    result.stderr,
                )
                assert [r[:2] for r in requests[start:] if r[1] == lost] == [
                    ("PUT", lost),
                    ("GET", lost),
                ]
                assert json.loads(result.stdout)["receipt"]["state"] == "completed"
                start = len(requests)
                result = subprocess.run(
                    cli + ["apply", "--request", ambiguous.split("/")[-2]],
                    capture_output=True,
                    check=False,
                    timeout=8,
                )
                assert result.returncode == 0, result.stderr
                assert [r[:2] for r in requests[start:] if r[1] == ambiguous] == [
                    ("PUT", ambiguous),
                    ("GET", ambiguous),
                ]
                result = subprocess.run(
                    cli + ["operation", "draft", "--request", draft.split("/")[-2]],
                    capture_output=True,
                    check=False,
                )
                assert result.returncode == 0, (
                    "draft creation must not dispatch",
                    result.stderr,
                )
                assert json.loads(result.stdout)["receipt"]["state"] == "draft"
                result = subprocess.run(
                    cli
                    + ["operation", "submit-draft", draft, "--if-match", '"version"'],
                    capture_output=True,
                    check=False,
                )
                assert result.returncode == 0, result.stderr
                assert json.loads(result.stdout)["receipt"]["state"] == "completed"
                result = subprocess.run(
                    cli + ["get", "/schema/"], capture_output=True, check=False
                )
                assert result.returncode == 0, (
                    "must negotiate the controller schema media type",
                    result.stderr,
                )
                assert json.loads(result.stdout) == {"type": "object"}
            finally:
                server.shutdown()
                worker.join()


if __name__ == "__main__":
    run(sys.argv[1])
