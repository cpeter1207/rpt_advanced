# SPDX-License-Identifier: GPL-2.0-only
"""Exercise the actual dynamic client against a product-created temporary graph."""

from __future__ import annotations

import fcntl
import json
import os
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

STORE = "31271e86-b1d8-490a-b576-008d69fa0428"


def main() -> None:
    """Run local-only operations; fixture keys are synthetic and never backed up."""
    client = Path(sys.argv[1]).resolve()
    archive = Path(sys.argv[2]).resolve()
    with tempfile.TemporaryDirectory(prefix="rptadv-store-client-") as temporary:
        root = Path(temporary)
        source = root / "source"
        source.mkdir(mode=0o700)
        with tarfile.open(archive) as bundle:
            bundle.extractall(source, filter="data")
        keys = root / "keys"
        keys.mkdir(mode=0o700)
        for name, byte in (("old", 9), ("new", 3)):
            key = keys / f"rpt-advanced.aead.{name}"
            key.write_bytes(bytes([byte]) * 32)
            key.chmod(0o400)
        keys.chmod(0o500)
        environment = dict(os.environ, CREDENTIALS_DIRECTORY=str(keys))

        def run(*arguments: str, success: bool = True) -> dict:
            result = subprocess.run(
                [str(client), *arguments],
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )
            assert (result.returncode == 0) == success, (
                result.returncode,
                result.stdout,
                result.stderr,
            )
            value = json.loads(result.stdout)
            assert value["status"] == ("complete" if success else "failed")
            return value

        initial = run("inspect", str(source), STORE)
        original_state = (source / "state.json").read_bytes()
        state = json.loads(original_state)
        assert state["store_uuid"] == STORE
        manifest_path = Path("manifests") / (state["desired"]["manifest"] + ".json")
        manifest = json.loads((source / manifest_path).read_bytes())
        objects = [
            Path(row["object"])
            for row in manifest["resources"].values()
            if "object" in row
        ]
        backup = root / "backup"
        backup.mkdir(mode=0o700)
        writer = os.open(source, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            fcntl.flock(writer, fcntl.LOCK_EX | fcntl.LOCK_NB)
            run("inspect", str(source), STORE)
            run("backup", str(source), STORE, str(backup))
            run("gc", str(source), STORE, initial["state_sha256"], success=False)
            assert (source / "state.json").read_bytes() == original_state
        finally:
            os.close(writer)
        assert (backup / "state.json").read_bytes() == original_state
        for relative in [manifest_path, *objects]:
            assert (backup / relative).read_bytes() == (source / relative).read_bytes()
        assert not (backup / "keys").exists()
        run(
            "retain",
            str(source),
            STORE,
            initial["state_sha256"],
            "missing",
            "[]",
            success=False,
        )
        assert (source / "state.json").read_bytes() == original_state
        run("retain", str(source), STORE, initial["state_sha256"], "old", "[]")
        restored = root / "restored"
        restored.mkdir(mode=0o700)
        result = run(
            "restore", str(restored), STORE, str(backup), "old", "none", "empty"
        )
        assert result["activation"] == "pending"
        before = json.loads((restored / "state.json").read_bytes())
        assert before["publication_journal"] is None
        assert before["desired"]["revision"] != state["desired"]["revision"]
        assert before["retained_recovery"]
        selected = run("inspect", str(restored), STORE)
        run(
            "rotate",
            str(restored),
            STORE,
            "old",
            "new",
            selected["state_sha256"],
        )
        after = json.loads((restored / "state.json").read_bytes())
        assert after["desired"]["revision"] == before["desired"]["revision"]
        assert after["activation"] == before["activation"]
        assert after["desired"] != before["desired"]
        assert before["desired"] in after["retained_recovery"]
        selected = run("inspect", str(restored), STORE)
        run("gc", str(restored), STORE, selected["state_sha256"])
        assert (restored / "working/site/metadata.json").read_bytes() == (
            source / "working/site/metadata.json"
        ).read_bytes()
        assert not (restored / "keys").exists()
        keys.chmod(0o700)
    print("Actual dynamic protected-store client passed temporary graph operations.")


if __name__ == "__main__":
    main()
