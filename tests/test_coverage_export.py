"""Coverage exports preserve every object and the complete measured keyset."""

import unittest

from export_rust_coverage import object_commands, reconcile


def report(count, true_count=0, false_count=0):
    """Small LLVM-format fixture with two executable lines and one branch."""
    return {
        "type": "llvm.coverage.json.export",
        "version": "2.0.1",
        "data": [
            {
                "files": [
                    {
                        "filename": "/source.rs",
                        "segments": [
                            [1, 1, count, True, True, False],
                            [3, 1, 0, False, False, False],
                        ],
                        "branches": [[1, 1, 1, 2, true_count, false_count, 0, 0, 4]],
                    }
                ]
            }
        ],
    }


class CoverageExportTests(unittest.TestCase):
    def test_every_object_retains_the_same_profile_and_filter(self):
        commands = object_commands(
            "Running `/tools/llvm-profdata merge profiles -o /run/one.profdata`\n"
            "Running `/tools/llvm-cov export -format=text "
            "-instr-profile=/run/one.profdata -object /run/a "
            "-object /run/b -ignore-filename-regex 'tests/|build.rs'`\n"
        )
        base = [
            "/tools/llvm-cov",
            "export",
            "-format=text",
            "-instr-profile=/run/one.profdata",
            "-ignore-filename-regex",
            "tests/|build.rs",
            "-object",
        ]
        self.assertEqual(commands, [base + ["/run/a"], base + ["/run/b"]])
        with self.assertRaises(ValueError):
            object_commands("Running `other-command export -object /run/a`")
        with self.assertRaises(ValueError):
            object_commands("Running `/tools/llvm-cov export -format=text`")

    def test_false_zero_aggregate_keeps_all_same_run_object_records(self):
        original = report(0)
        first, second = report(1, true_count=1), report(0, false_count=1)
        combined = reconcile(original, [first, second])
        self.assertEqual(combined["data"], first["data"] + second["data"])
        self.assertEqual(combined["type"], original["type"])
        self.assertEqual(original["data"][0]["files"][0]["segments"][0][2], 0)

    def test_missing_or_added_measured_keys_are_rejected(self):
        changed = report(1)
        changed["data"][0]["files"][0]["branches"] = []
        with self.assertRaises(ValueError):
            reconcile(report(0), [changed])
        changed = report(1)
        changed["data"][0]["files"][0]["segments"][1][0] = 4
        with self.assertRaises(ValueError):
            reconcile(report(0), [changed])

    def test_other_report_versions_cannot_be_mixed(self):
        changed = report(1)
        changed["version"] = "other"
        with self.assertRaises(ValueError):
            reconcile(report(0), [changed])


if __name__ == "__main__":
    unittest.main()
