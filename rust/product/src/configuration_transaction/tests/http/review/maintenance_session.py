"""Exercise temporary controls only on the product harness's synthetic radio."""

import http.client
import json
import ssl
import subprocess
import sys
import time
import uuid
from pathlib import Path

binary, keys, listener, port = sys.argv[1:]
keys = Path(keys)
api = "/api/v1"
op_port = port.rstrip("/").split("/")[-1]
url = f"https://localhost:{listener}{api}/"
cli = [
    binary,
    "--url",
    url,
    "--ca",
    str(keys / "backend.pem"),
    "--cert",
    str(keys / "cli.pem"),
    "--key",
    str(keys / "cli.key"),
]
actions = [
    {"operation": "get_mixer", "target": "receive"},
    {"operation": "read_eeprom"},
    {"operation": "set_test_tone", "enabled": True},
    {"operation": "set_ctcss_inhibit", "enabled": True},
    {"operation": "set_subaudible_override", "enabled": True},
    {"operation": "transmit", "keyed": True},
]
session = subprocess.run(
    cli + ["tune", port],
    input="".join(json.dumps(a) + "\n" for a in actions) + "quit\n",
    text=True,
    check=False,
    capture_output=True,
    timeout=24,
)
assert session.returncode == 0, session.stderr
docs = [json.loads(line) for line in session.stdout.splitlines()]
assert docs[0]["receipt"]["outcomes"][0]["result"]["level"] == 317
assert docs[1]["receipt"]["outcomes"][0]["result"]["words"] == [0] * 63 + [925]
assert all(d["receipt"]["state"] == "completed" for d in docs[: len(actions)])
ctx = ssl.create_default_context(cafile=str(keys / "backend.pem"))
ctx.load_cert_chain(keys / "cli.pem", keys / "cli.key")


def connect():
    return http.client.HTTPSConnection(
        "localhost", int(listener), context=ctx, timeout=7
    )


def request(c, method, path, status, body=None, conditional=None):
    headers = {"Accept": "application/hal+json", "Host": "localhost"}
    if body is not None:
        headers["Content-Type"] = (
            "application/json-patch+json"
            if method == "PATCH"
            else "application/hal+json"
        )
    if conditional:
        headers[conditional[0]] = conditional[1]
    c.request(method, path, json.dumps(body) if body is not None else None, headers)
    out = c.getresponse()
    doc = json.loads(out.read())
    assert out.status == status, (method, path, status, out.status, doc)
    return doc, out.getheader("ETag")


one, two = connect(), connect()
revision = request(one, "GET", api + "/status/active/", 200)[0]["active_revision"]
collection = request(one, "GET", port, 200)[0]["_links"]["rpt:maintenance"]["href"]
template = request(one, "GET", collection, 200)[0]["_links"]["rpt:lease"]["href"]
op = request(one, "GET", api + "/operations/", 200)[0]["_links"]["rpt:request"]["href"]
create = ("If-None-Match", "*")
for ending in ("expiry", "disconnect", "inactive", "disable"):
    lease = template.replace("{lease}", str(uuid.uuid4()))
    cfg = {
        "configuration": {
            "active": True,
            "expected_active_revision": revision,
            "ttl_ms": 30000,
        }
    }
    request(one, "PUT", lease, 428, cfg)
    request(one, "PUT", lease, 201, cfg, create)
    _, tag = request(one, "GET", lease, 200)
    request(two, "DELETE", lease, 403, conditional=("If-Match", tag))
    request(
        one,
        "PATCH",
        lease,
        412,
        [{"op": "replace", "path": "/configuration/ttl_ms", "value": 30000}],
        ("If-Match", '"stale"'),
    )
    request(
        one,
        "PATCH",
        lease,
        200,
        [{"op": "replace", "path": "/configuration/ttl_ms", "value": 30000}],
        ("If-Match", tag),
    )
    command = {
        "configuration": {
            "submitted": True,
            "expected_active_revision": revision,
            "targets": [op_port],
            "action": {
                "action": "radio_maintenance",
                "operation": "transmit",
                "keyed": True,
            },
            "maintenance_lease": lease,
        }
    }
    attempt = op.replace("{request}", str(uuid.uuid4()))
    request(two, "PUT", attempt, 403, command, create)
    assert (
        request(one, "PUT", attempt, 200, command, create)[0]["receipt"]["state"]
        == "completed"
    )
    assert (
        request(one, "PUT", attempt, 200, command, create)[0]["receipt"]["state"]
        == "completed"
    )
    if ending == "disconnect":
        one.close()
        one = connect()
    elif ending == "disable":
        _, tag = request(two, "GET", port, 200)
        request(
            two,
            "PUT",
            port,
            202,
            {
                "configuration": {
                    "name": "Radio",
                    "inherit_from": "site",
                    "enabled": False,
                }
            },
            ("If-Match", tag),
        )
        until = time.monotonic() + 8
        while True:
            health = request(two, "GET", api + "/health/", 200)[0]["status"]
            if (
                health["historical_activation"] == "confirmed"
                and health["active_revision"] != revision
            ):
                break
            assert time.monotonic() < until, "disabled port must retire old owner"
            time.sleep(0.02)
    else:
        _, tag = request(one, "GET", lease, 200)
        leaf, value = ("active", False) if ending == "inactive" else ("ttl_ms", 50)
        request(
            one,
            "PATCH",
            lease,
            200,
            [{"op": "replace", "path": "/configuration/" + leaf, "value": value}],
            ("If-Match", tag),
        )
    until = time.monotonic() + 4
    while request(two, "GET", lease, 200)[0]["configuration"]["active"]:
        assert time.monotonic() < until, "terminal lease must be visible"
        time.sleep(0.02)
    request(
        one, "PUT", op.replace("{request}", str(uuid.uuid4())), 403, command, create
    )
one.close()
two.close()
print(
    "MAINTENANCE_SESSION cli_queries=true socket_bound=true stale_etag=true replay=true expired=true disconnected=true cancelled=true retired=true"
)
