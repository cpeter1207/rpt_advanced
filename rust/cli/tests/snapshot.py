"""Check paged snapshots through the operator discovery fixture's TLS listener."""

import json
import subprocess
from copy import deepcopy
from urllib.parse import urlencode

SNAPSHOT = "/fixture/captures/status/"


def check_snapshots(cli, calls, documents, root, status):
    """Verify selector encoding, response assembly and fail-closed pagination."""
    port = "11111111-1111-4111-8111-111111111111"
    disabled = "22222222-2222-4222-8222-222222222222"
    continuation = f"{SNAPSHOT}?cursor=opaque"
    query = urlencode(
        [
            ("site", "true"),
            ("port", port),
            ("port", disabled),
            ("topics", "radio,meters"),
        ]
    )
    selected = f"{SNAPSHOT}?{query}"
    bad_filter = f"{SNAPSHOT}?topics=not-a-topic"
    metadata = {
        "boot_id": "33333333-3333-4333-8333-333333333333",
        "snapshot_id": "44444444-4444-4444-8444-444444444444",
        "state_cursor": "27",
        "active_revision": "active-generation",
        "observed_at": "2026-11-26T20:00:00.000Z",
        "monotonic_ms": "125",
        "page_index": 0,
        "complete": True,
    }
    single = {
        **metadata,
        "observed_at": None,
        "selection": {"site": True, "ports": [], "topics": ["radio"]},
        "status": {"site": {"quiet": True}},
        "_links": {"self": {"href": SNAPSHOT}},
    }
    first = {
        **metadata,
        "selection": {
            "site": True,
            "ports": [port, disabled],
            "topics": ["radio", "meters"],
        },
        "complete": False,
        "status": {
            "site": {"quiet": True},
            "ports": {
                port: {"active": True, "meters": {"rx/mono": {"rms_dbfs": None}}},
                disabled: {"active": False},
            },
        },
        "_links": {"self": {"href": selected}, "next": {"href": continuation}},
    }
    second = {
        **metadata,
        "selection": first["selection"],
        "page_index": 1,
        "status": {
            "ports": {
                port: {"active": True, "meters": {"tx/mono": {"rms_dbfs": -23.5}}}
            }
        },
        "_links": {"self": {"href": continuation}},
    }
    complete = deepcopy(first)
    complete["complete"] = True
    del complete["_links"]["next"]
    complete["status"]["ports"][port]["meters"]["tx/mono"] = {"rms_dbfs": -23.5}
    documents.update(
        {SNAPSHOT: single, selected: first, continuation: second, bad_filter: 400}
    )
    selectors = [
        "--site",
        "--port",
        port,
        "--port",
        disabled,
        "--topics",
        "radio,meters",
    ]

    def snapshot(args, pages, body=None, error=None):
        start = len(calls)
        result = subprocess.run(
            cli + ["status", "snapshot"] + args,
            capture_output=True,
            timeout=5,
            check=False,
        )
        if error is None:
            assert result.returncode == 0, result.stderr.decode(errors="replace")
            assert json.loads(result.stdout) == body
        else:
            assert result.returncode and not result.stdout, result
            assert error in result.stderr.decode(errors="replace"), result.stderr
        assert calls[start:] == [(path, "cli") for path in [root, status] + pages]

    snapshot([], [SNAPSHOT], body=single)
    missing_time = {key: value for key, value in single.items() if key != "observed_at"}
    for invalid in (missing_time, {**single, "observed_at": 0}):
        documents[SNAPSHOT] = invalid
        snapshot([], [SNAPSHOT], error="invalid snapshot metadata")
    documents[SNAPSHOT] = single

    snapshot(selectors, [selected, continuation], body=complete)
    snapshot(["--topics", "not-a-topic"], [bad_filter], error="HTTP 400")
    documents[continuation] = 410
    snapshot(selectors, [selected, continuation], error="expired")
    for key, value in (
        ("boot_id", "different-boot"),
        ("snapshot_id", "different-capture"),
        ("state_cursor", "28"),
        ("observed_at", None),
        ("selection", {"site": False, "ports": [port], "topics": ["radio"]}),
    ):
        documents[continuation] = {**second, key: value}
        snapshot(selectors, [selected, continuation], error="different capture")
    documents[continuation] = {**second, "page_index": 3}
    snapshot(selectors, [selected, continuation], error="page index")
    documents[continuation] = {**second, "status": {"ports": {port: {"active": False}}}}
    snapshot(selectors, [selected, continuation], error="conflicting snapshot record")
    first["_links"]["next"]["href"] = "//others.example/capture"
    snapshot(selectors, [selected], error="origin-relative HAL link")
    first["_links"]["next"]["href"] = selected
    snapshot(selectors, [selected], error="cyclic snapshot")
    del first["_links"]["next"]
    snapshot(selectors, [selected], error="missing snapshot continuation")

    result = subprocess.run(
        [cli[0], "--help"], capture_output=True, timeout=5, check=True
    )
    assert b"status snapshot" in result.stdout
