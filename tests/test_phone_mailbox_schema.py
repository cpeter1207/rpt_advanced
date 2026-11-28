"""Check mailbox access lists in the canonical public phone and snapshot schemas."""

import json
import unittest
from pathlib import Path

from jsonschema import Draft202012Validator


class PhoneMailboxSchemaTest(unittest.TestCase):
    def test_public_readback_preserves_exact_mailbox_ids(self):
        root = Path(__file__).resolve().parents[1]
        schema = json.loads((root / "repeater-management.schema.json").read_text())
        phone = {"allow_mailboxes": ["0741", "741"], "deny_mailboxes": ["741"]}
        for name, document in (
            ("publicPhonePatch", phone),
            ("siteSnapshot", {"phone_patch": phone}),
            ("nodeSnapshot", {"phone_patch": phone}),
        ):
            with self.subTest(schema=name):
                validator = Draft202012Validator(
                    {"$ref": f"#/$defs/{name}", "$defs": schema["$defs"]}
                )
                validator.validate(document)


if __name__ == "__main__":
    unittest.main()
