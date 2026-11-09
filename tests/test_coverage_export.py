"""Coverage exports preserve every object and the complete measured keyset."""

import json
import tempfile
import unittest
from copy import deepcopy
from pathlib import Path

from export_rust_coverage import object_commands, reconcile
from test_rust_product_surface import coverage


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
                ],
                "functions": [
                    {
                        "name": "fixture",
                        "filenames": ["/source.rs"],
                        "regions": [[1, 1, 3, 1, count, 0, 0, 0]],
                        "branches": [[1, 1, 1, 2, true_count, false_count, 0, 0, 4]],
                    }
                ],
            }
        ],
    }


def decision_report(*copies):
    """LLVM-shaped copies retain function starts and each repeated decision."""
    unit = report(1, 1, 1)["data"][0]
    unit["functions"] = []
    unit["files"][0]["branches"] = []
    for index, (start, end, counts) in enumerate(copies):
        branches = [[2, 1, 2, 8, *arms, 0, 0, 4] for arms in counts]
        unit["functions"].append(
            {
                "name": f"compiled_hash_{index}",
                "filenames": ["/source.rs"],
                "regions": [[*start, *end, 1, 0, 0, 0]],
                "branches": branches,
            }
        )
        unit["files"][0]["branches"].extend(deepcopy(branches))
    return {"type": "llvm.coverage.json.export", "version": "2.0.1", "data": [unit]}


class CoverageDecisionTests(unittest.TestCase):
    def validate(self, value):
        """Exercise the real production coverage gate using a serialized report."""
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "coverage.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            coverage(path)

    def test_repeated_span_siblings_cannot_borrow_each_others_hits(self):
        value = decision_report(
            ((1, 1), (3, 1), [(1, 1), (1, 0)]),
            ((1, 1), (3, 1), [(0, 0), (0, 0)]),
        )
        with self.assertRaisesRegex(AssertionError, "branch coverage below 100%"):
            self.validate(value)
        value["data"][0]["functions"][1]["branches"][1][5] = 1
        self.validate(value)

    def test_compiled_copies_share_a_start_even_when_region_ends_differ(self):
        self.validate(
            decision_report(
                ((1, 1), (3, 1), [(1, 0)]),
                ((1, 1), (2, 9), [(0, 1)]),
            )
        )

    def test_distinct_function_starts_do_not_share_branch_hits(self):
        with self.assertRaisesRegex(AssertionError, "branch coverage below 100%"):
            self.validate(
                decision_report(
                    ((1, 1), (3, 1), [(1, 0)]),
                    ((1, 2), (3, 1), [(0, 1)]),
                )
            )

    def test_file_branches_require_function_metadata(self):
        value = report(1, 1, 1)
        del value["data"][0]["functions"]
        with self.assertRaisesRegex(ValueError, "function branch metadata"):
            self.validate(value)

    def test_partial_metadata_cannot_omit_a_reported_branch_span(self):
        value = decision_report(((1, 1), (3, 1), [(1, 1)]))
        value["data"][0]["files"][0]["branches"].append([2, 2, 2, 8, 1, 1, 0, 0, 4])
        with self.assertRaisesRegex(ValueError, "function branch metadata"):
            self.validate(value)

    def test_another_same_run_object_can_supply_function_metadata(self):
        value = decision_report(((1, 1), (3, 1), [(1, 1)]))
        hash_zero = deepcopy(value["data"][0])
        del hash_zero["functions"]
        value["data"].insert(0, hash_zero)
        self.validate(value)

    def test_function_metadata_can_live_in_an_empty_file_unit(self):
        value = decision_report(((1, 1), (3, 1), [(1, 1)]))
        metadata = deepcopy(value["data"][0])
        metadata["files"] = []
        del value["data"][0]["functions"]
        value["data"].append(metadata)
        self.validate(value)

    def test_dependencies_outside_report_files_do_not_expand_scope(self):
        value = decision_report(((1, 1), (3, 1), [(1, 1)]))
        dependency = deepcopy(value["data"][0]["functions"][0])
        dependency["filenames"] = ["/registry/dependency.rs"]
        dependency["branches"][0][4:6] = [0, 0]
        value["data"][0]["functions"].append(dependency)
        self.validate(value)

    def test_function_file_ids_select_the_actual_branch_source(self):
        value = decision_report(((1, 1), (3, 1), [(1, 1)]))
        function = value["data"][0]["functions"][0]
        function["filenames"].insert(0, "/registry/dependency.rs")
        function["regions"][0][5] = 1
        function["branches"][0][6] = 1
        self.validate(value)

    def test_export_rejects_lost_same_span_decision_metadata(self):
        original = decision_report(((1, 1), (3, 1), [(1, 1), (1, 1)]))
        changed = deepcopy(original)
        changed["data"][0]["functions"][0]["branches"].pop()
        with self.assertRaises(ValueError):
            reconcile(original, [changed])

    def test_export_merges_extra_compiled_copies_without_losing_decisions(self):
        original = decision_report(((1, 1), (3, 1), [(1, 1), (1, 1)]))
        zero_copy = decision_report(((1, 1), (2, 9), [(0, 0), (0, 0)]))
        combined = reconcile(original, [zero_copy, original])
        self.assertEqual(combined["data"], zero_copy["data"] + original["data"])


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

    def test_missing_aggregate_function_branches_use_exact_object_metadata(self):
        original = report(0)
        original["data"][0]["functions"][0]["branches"] = []
        before = deepcopy(original)
        first, second = report(1, 1, 0), report(2, 0, 1)
        combined = reconcile(original, [first, second])
        self.assertEqual(combined["data"], first["data"] + second["data"])
        self.assertEqual(original, before)

    def test_missing_aggregate_metadata_cannot_hide_an_integration_line(self):
        original = report(0)
        original["data"][0]["functions"][0]["branches"] = []
        integration = {
            "filename": "/workspace/rust/product/src/link/session.rs",
            "segments": [
                [892, 1, 0, True, True, False],
                [893, 1, 0, False, False, False],
            ],
            "branches": [],
        }
        original["data"][0]["files"].append(integration)
        complete = report(1, 1, 1)
        complete["data"][0]["files"].append(deepcopy(integration))
        self.assertEqual(reconcile(original, [complete])["data"], complete["data"])
        with self.assertRaisesRegex(ValueError, "measured line/branch keyset"):
            reconcile(original, [report(1, 1, 1)])

    def test_missing_aggregate_metadata_requires_unique_structural_support(self):
        original = report(0)
        original["data"][0]["functions"][0]["branches"] = []
        for mutation in [
            "missing_function",
            "missing_source",
            "different_source",
            "ambiguous",
        ]:
            with self.subTest(mutation=mutation):
                baseline = deepcopy(original)
                complete = report(1, 1, 1)
                candidates = [complete]
                if mutation == "missing_function":
                    baseline["data"][0]["functions"] = []
                elif mutation == "missing_source":
                    baseline["data"][0]["functions"][0]["regions"] = []
                elif mutation == "different_source":
                    complete["data"][0]["functions"][0]["regions"][0][1] = 2
                else:
                    sibling = deepcopy(complete)
                    sibling["data"][0]["functions"][0]["branches"].append(
                        [1, 1, 1, 2, 0, 0, 0, 0, 4]
                    )
                    candidates.append(sibling)
                with self.assertRaises(ValueError):
                    reconcile(baseline, candidates)

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
