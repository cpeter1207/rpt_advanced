#!/usr/bin/env python3
"""Run the disposable coturn fixture on a private loopback-only network."""

import base64
import hmac
import os
import secrets
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

PORT = 3478
TLS_PORT = 5349


def openssl(*args, directory):
    subprocess.run(
        ["openssl", *args], cwd=directory, stdout=subprocess.DEVNULL, check=True
    )


def pki(directory):
    openssl(
        "req",
        "-new",
        "-newkey",
        "ec",
        "-pkeyopt",
        "ec_paramgen_curve:P-256",
        "-nodes",
        "-x509",
        "-days",
        "1",
        "-subj",
        "/CN=TURN fixture root",
        "-addext",
        "basicConstraints=critical,CA:TRUE",
        "-addext",
        "keyUsage=critical,keyCertSign",
        "-keyout",
        "ca.key",
        "-out",
        "ca.pem",
        directory=directory,
    )
    openssl(
        "req",
        "-new",
        "-newkey",
        "ec",
        "-pkeyopt",
        "ec_paramgen_curve:P-256",
        "-nodes",
        "-subj",
        "/CN=localhost",
        "-keyout",
        "server.key",
        "-out",
        "server.csr",
        directory=directory,
    )
    (directory / "server.ext").write_text(
        "basicConstraints=critical,CA:FALSE\n"
        "keyUsage=critical,digitalSignature\n"
        "extendedKeyUsage=serverAuth\n"
        "subjectAltName=DNS:localhost\n",
        encoding="ascii",
    )
    openssl(
        "x509",
        "-req",
        "-in",
        "server.csr",
        "-CA",
        "ca.pem",
        "-CAkey",
        "ca.key",
        "-CAcreateserial",
        "-days",
        "1",
        "-extfile",
        "server.ext",
        "-out",
        "server.pem",
        directory=directory,
    )
    openssl(
        "x509",
        "-in",
        "ca.pem",
        "-outform",
        "DER",
        "-out",
        "ca.der",
        directory=directory,
    )


def start_configuration(directory):
    os.umask(0o077)
    pki(directory)
    secret = secrets.token_hex(32)
    config = directory / "turnserver.conf"
    config.write_text(
        f"listening-port={PORT}\n"
        f"tls-listening-port={TLS_PORT}\n"
        "listening-ip=127.0.0.1\n"
        "relay-ip=127.0.0.1\n"
        "min-port=49160\n"
        "max-port=49175\n"
        "realm=turn-fixture.test\n"
        "no-cli\n"
        "no-dtls\n"
        "no-tcp-relay\n"
        "allow-loopback-peers\n"
        "no-multicast-peers\n"
        "relay-threads=1\n"
        "fingerprint\n"
        "use-auth-secret\n"
        f"static-auth-secret={secret}\n"
        f"cert={directory / 'server.pem'}\n"
        f"pkey={directory / 'server.key'}\n"
        f"pidfile={directory / 'turnserver.pid'}\n"
        f"userdb={directory / 'turn.sqlite'}\n"
        "simple-log\n"
        "log-file=stdout\n",
        encoding="ascii",
    )
    expiry = int(time.time()) + 120
    for leg in ("udp", "tcp", "tls"):
        user = f"{expiry}:fixture-{leg}-{secrets.token_hex(4)}"
        password = base64.b64encode(hmac.digest(secret.encode(), user.encode(), "sha1"))
        (directory / f"{leg}.credentials").write_bytes(
            user.encode() + b"\n" + password + b"\n"
        )
    return config


def wait_ready(server):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline and server.poll() is None:
        try:
            with (
                socket.create_connection(("127.0.0.1", PORT), timeout=0.1),
                socket.create_connection(("127.0.0.1", TLS_PORT), timeout=0.1),
            ):
                return
        except OSError:
            time.sleep(0.03)
    raise RuntimeError("isolated TURN listener never became ready")


def inside(turnserver, test):
    interfaces = {
        line.split(":", 1)[0].strip()
        for line in Path("/proc/net/dev").read_text().splitlines()[2:]
    }
    if interfaces != {"lo"}:
        raise RuntimeError("refusing to start coturn outside a loopback-only namespace")
    subprocess.run(["ip", "link", "set", "dev", "lo", "up"], check=True)
    with tempfile.TemporaryDirectory(prefix="rpt-turn-probe-") as temporary:
        directory = Path(temporary)
        config = start_configuration(directory)
        with (directory / "coturn.log").open("wb") as log:
            server = subprocess.Popen(
                [turnserver, "-c", config],
                cwd=directory,
                stdout=log,
                stderr=subprocess.STDOUT,
            )
            try:
                wait_ready(server)
                subprocess.run(
                    [test, "--ignored", "--nocapture"],
                    env={**os.environ, "RPTADV_TURN_FIXTURE_DIR": str(directory)},
                    check=True,
                    timeout=30,
                )
            finally:
                if server.poll() is None:
                    server.terminate()
                    try:
                        server.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        server.kill()
                        server.wait(timeout=3)


def main():
    if len(sys.argv) == 4 and sys.argv[1] == "--inside":
        inside(sys.argv[2], sys.argv[3])
        return
    if len(sys.argv) != 3:
        raise SystemExit(f"usage: {sys.argv[0]} /path/to/turnserver /path/to/turn-test")
    turnserver, test = (str(Path(arg).resolve(strict=True)) for arg in sys.argv[1:])
    subprocess.run(
        [
            "unshare",
            "--user",
            "--map-root-user",
            "--net",
            "--pid",
            "--fork",
            "--kill-child=SIGKILL",
            sys.executable,
            Path(__file__).resolve(),
            "--inside",
            turnserver,
            test,
        ],
        check=True,
        timeout=45,
    )


if __name__ == "__main__":
    main()
