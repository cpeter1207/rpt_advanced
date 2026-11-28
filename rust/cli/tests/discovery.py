"""Check the Rust client follows the authenticated root's actual status link."""

import json
import ssl
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

from snapshot import SNAPSHOT, check_snapshots

ROOT = "/fixture/v2/"
STATUS = "/fixture/current/observation/"
RESPONSE = {
    "status": {"active_revision": "observed-revision"},
    "_links": {"rpt:status-snapshot": {"href": SNAPSHOT}},
}


def check(binary):
    """Require a verified client and a GET to the server-selected resource."""
    pki = Path(__file__).resolve().parents[2] / "peer-support/tests/https-pki.sh"
    with tempfile.TemporaryDirectory() as directory:
        tmp = Path(directory)
        subprocess.run(["sh", str(pki), directory], check=True, capture_output=True)
        calls = []
        documents = {
            ROOT: {"_links": {"rpt:status": {"href": STATUS}}},
            STATUS: RESPONSE,
        }

        class Hal(BaseHTTPRequestHandler):
            """Serve an independently selected link and record the peer credential."""

            def do_GET(self):
                """Record the authenticated peer and return the selected representation."""
                subject = self.connection.getpeercert()["subject"]
                common_name = dict(item for group in subject for item in group)[
                    "commonName"
                ]
                calls.append((self.path, common_name))
                document = documents.get(self.path, 404)
                if isinstance(document, int):
                    self.send_error(document)
                    return
                body = json.dumps(document).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/hal+json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, _format, *_args):
                """Keep HTTP tracing out of the test output."""

        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.load_cert_chain(tmp / "server.pem", tmp / "server.key")
        tls.load_verify_locations(tmp / "client.pem")
        tls.verify_mode = ssl.CERT_REQUIRED
        with HTTPServer(("127.0.0.1", 0), Hal) as server:
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
                result = subprocess.run(
                    cli + ["get", "rpt:status"],
                    capture_output=True,
                    timeout=10,
                    check=False,
                )
                before = list(calls)
                for change in (
                    {"--ca": str(tmp / "foreign.pem")},
                    {
                        "--cert": str(tmp / "stranger.pem"),
                        "--key": str(tmp / "stranger.key"),
                    },
                    {"--url": f"https://127.0.0.1:{server.server_port}{ROOT}"},
                ):
                    rejected = cli.copy()
                    for flag, value in change.items():
                        rejected[rejected.index(flag) + 1] = value
                    denial = subprocess.run(
                        rejected + ["get"], capture_output=True, timeout=5, check=False
                    )
                    assert denial.returncode and not denial.stdout and calls == before
                foreign = subprocess.run(
                    cli + ["get", "//others.example/"],
                    capture_output=True,
                    timeout=5,
                    check=False,
                )
                assert foreign.returncode and not foreign.stdout and calls == before
                assert result.returncode == 0, result.stderr.decode(errors="replace")
                assert calls == [(ROOT, "cli"), (STATUS, "cli")], (
                    "expected authenticated GET of root and advertised status link",
                    calls,
                )
                assert json.loads(result.stdout) == RESPONSE
                check_snapshots(cli, calls, documents, ROOT, STATUS)
            finally:
                server.shutdown()
                worker.join()


if __name__ == "__main__":
    check(sys.argv[1])
