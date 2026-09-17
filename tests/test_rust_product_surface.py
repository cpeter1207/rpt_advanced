#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Reject legacy production C and static-library installation after the Rust cutover."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import tempfile
from itertools import pairwise
from pathlib import Path
from unittest.mock import patch

LOADER = Path("module/app_rpt_advanced_loader.c")
FORBIDDEN_MAKE_TOKENS = (
    "librpt_advanced.a",
    "$(wildcard src/*.c)",
)
ADAPTERS = ("asterisk", "control_asterisk", "file", "speech")
PRODUCT = "librptadv_product.so.1"
RING_MINIMUM_VERSION = "2.0.0~alpha2"
LIBRARIES = {PRODUCT, *(f"librptadv_{name}_adapter.so.1" for name in ADAPTERS)}
ELF_DEPENDENCIES = {
    "app_rpt_advanced.so": LIBRARIES,
    "librptadv_asterisk_adapter.so.1": set(),
    PRODUCT: {
        "librate_adjusting_pcm_ring2.so.2",
        "librptadv_samplerate_adapter.so.1",
    },
    "librptadv_control_asterisk_adapter.so.1": set(),
    "librptadv_file_adapter.so.1": set(),
    "librptadv_speech_adapter.so.1": set(),
}
EXPORTS = {
    "librptadv_asterisk_adapter.so.1": "rptadv_asterisk_descriptor_v1",
    PRODUCT: "rptadv_product_descriptor_v1",
    "librptadv_control_asterisk_adapter.so.1": "rptadv_control_descriptor_v1",
    "librptadv_file_adapter.so.1": "rptadv_file_adapter_descriptor",
    "librptadv_speech_adapter.so.1": "rptadv_speech_adapter_descriptor",
}
MANUALS = (
    "README.md",
    "QUALITY.md",
    "AGENTS.md",
    "WISHLIST.md",
    "COPYING",
    *(
        path.relative_to(Path(__file__).resolve().parents[1]).as_posix()
        for pattern in (
            "doc/*.md",
            "doc/architecture/*.md",
            "doc/architecture/decisions/*.md",
        )
        for path in sorted(Path(__file__).resolve().parents[1].glob(pattern))
    ),
)


def command(*arguments: str) -> str:
    """Read one native artifact using the platform's inspection tool."""
    return subprocess.run(arguments, check=True, capture_output=True, text=True).stdout


def dynamic(path: Path, tag: str) -> set[str]:
    """Return exact ELF dynamic-string values for one tag."""
    return set(
        re.findall(rf"\({tag}\).*\[([^\]]+)\]", command("readelf", "-d", str(path)))
    )


def artifacts(directory: Path, runpath: str = "$ORIGIN/../../rpt_advanced") -> None:
    """Check versioned dynamic composition and reject duplicated implementations."""
    system = {
        "libc.so.6",
        "libm.so.6",
        "libgcc_s.so.1",
        "ld-linux-x86-64.so.2",
        "ld-linux-aarch64.so.1",
    }
    for name, dependencies in ELF_DEPENDENCIES.items():
        path = directory / name
        assert path.is_file(), f"missing artifact: {path}"
        if name in LIBRARIES:
            assert dynamic(path, "SONAME") == {name}, f"incorrect SONAME: {name}"
            exports = {
                line.split()[-1]
                for line in command(
                    "nm", "-D", "--defined-only", str(path)
                ).splitlines()
            }
            assert exports == {EXPORTS[name]}, (
                f"incorrect exports for {name}: {exports}"
            )
        needed = dynamic(path, "NEEDED")
        assert needed - system == dependencies, f"incorrect NEEDED for {name}: {needed}"
        symbols = command("nm", "--defined-only", "--demangle", str(path))
        assert not re.search(
            r"\b(rpcr2_descriptor|rptadv_samplerate_adapter_descriptor)$",
            symbols,
            re.MULTILINE,
        ), f"static external adapter in {name}"
        if name != PRODUCT:
            assert "rpt_advanced_core::" not in symbols, f"duplicated core in {name}"
    assert dynamic(directory / "app_rpt_advanced.so", "RUNPATH") == {runpath}, (
        "loader must resolve the configured private-library directory"
    )


def staged(
    root: Path, stage: Path, multiarch: str, module_directory: Path | None = None
) -> None:
    """Require an exact runtime/development manifest and source-identical documents."""
    library = Path(f"usr/lib/{multiarch}/rpt_advanced")
    module = (
        module_directory.relative_to("/")
        if module_directory is not None
        else Path(f"usr/lib/{multiarch}/asterisk/modules")
    ) / "app_rpt_advanced.so"
    expected = {module, *(library / name for name in LIBRARIES)}
    link = library / "librptadv_product.so"
    expected.add(link)
    assert (stage / link).is_symlink(), f"missing developer link: {link}"
    assert (stage / link).readlink() == Path(PRODUCT)
    header = Path("usr/include/rptadv_product.h")
    expected.add(header)
    assert (stage / header).read_bytes() == (
        root / "rust/product/include/rptadv_product.h"
    ).read_bytes()
    for name, crate in (("control_asterisk", "control-asterisk-adapter"),):
        link = library / f"librptadv_{name}_adapter.so"
        expected.add(link)
        assert (stage / link).is_symlink(), f"missing developer link: {link}"
        assert (stage / link).readlink() == Path(f"librptadv_{name}_adapter.so.1")
        header = Path(f"usr/include/rptadv_{name}_adapter.h")
        expected.add(header)
        assert (stage / header).read_bytes() == (
            root / "rust" / crate / "include" / header.name
        ).read_bytes()
    for name, crate in (("file", "file-adapter"), ("speech", "speech-adapter")):
        link = library / f"librptadv_{name}_adapter.so"
        expected.add(link)
        assert (stage / link).is_symlink(), f"missing developer link: {link}"
        assert (stage / link).readlink() == Path(f"librptadv_{name}_adapter.so.1")
        directory = Path(f"usr/include/rpt_advanced/{name}")
        sources = (
            (
                f"rptadv_{name}_adapter.h",
                root / "rust" / crate / "include" / f"rptadv_{name}_adapter.h",
            ),
            (
                "rptadv_media_types.h",
                root / "rust/media-support/include/rptadv_media_types.h",
            ),
        )
        for header, source in sources:
            destination = directory / header
            expected.add(destination)
            assert (stage / destination).read_bytes() == source.read_bytes()
    doc = Path("usr/share/doc/rpt-advanced")
    documents = {doc / path: root / path for path in MANUALS}
    documents[doc / "copyright"] = root / "COPYING"
    documents[doc / "examples/rpt_advanced.conf"] = root / "examples/rpt_advanced.conf"
    for destination, source in documents.items():
        expected.add(destination)
        assert (stage / destination).read_bytes() == source.read_bytes(), str(
            destination
        )
    found = {
        path.relative_to(stage)
        for path in stage.rglob("*")
        if path.is_file() or path.is_symlink()
    }
    assert found == expected, (
        f"staged manifest: missing={expected - found}, extra={found - expected}"
    )
    for name in LIBRARIES:
        assert (stage / library / name).read_bytes() == (
            root / "build" / name
        ).read_bytes()
    assert (stage / module).read_bytes() == (
        root / "build/app_rpt_advanced.so"
    ).read_bytes()
    resolved = command(
        "env", "-u", "LD_LIBRARY_PATH", "ldd", str((stage / module).resolve())
    )
    assert "not found" not in resolved, f"unresolved staged dependency: {resolved}"
    paths = dict(re.findall(r"(\S+) => (\S+)", resolved))
    for name in LIBRARIES:
        assert Path(paths[name]).resolve() == (stage / library / name).resolve(), (
            f"loader resolved {name} outside the staged private directory"
        )


def package(control: Path) -> None:
    """Require runtime dependencies that provide this product's complete ABI table."""
    contents = control.read_text(encoding="utf-8")
    for token in (
        "Package: rpt-advanced",
        "cargo",
        "rustc",
        "debhelper",
        "pkg-config",
        "dh-sequence-asterisk",
        "librate-adjusting-pcm-ring2-dev",
        "librptadv-samplerate-adapter-dev",
        "${shlibs:Depends}",
        "${misc:Depends}",
        "${asterisk:Depends}",
    ):
        assert token in contents, f"missing package dependency: {token}"
    for token in (
        f"librate-adjusting-pcm-ring2-dev (>= {RING_MINIMUM_VERSION})",
        f"librate-adjusting-pcm-ring2 (>= {RING_MINIMUM_VERSION})",
    ):
        assert token in contents, f"missing compatible ring provider: {token}"
    assert "asl3-asterisk" not in contents, "ASL3-specific package dependency"


def coverage(report: Path) -> None:
    """Require every executable source line/branch, unioning duplicate LLVM instances."""
    data = json.loads(report.read_text(encoding="utf-8"))
    lines = {}
    branches = {}
    assert data["data"], "missing coverage instrumentation"
    for unit in data["data"]:
        for source in unit["files"]:
            segments = source.get("segments", [])
            if segments:
                assert not segments[-1][3] or segments[-1][5], (
                    "unterminated executable coverage segment"
                )
            for segment, following in pairwise(segments):
                assert segment[:2] <= following[:2], "unordered coverage segments"
                if not segment[3] or segment[5] or segment[:2] == following[:2]:
                    continue
                assert segment[2] >= 0, "invalid line execution count"
                # LLVM segments describe half-open source intervals. An end at
                # column 1 does not execute that next line; gap regions do not
                # turn comments/braces between code regions into requirements.
                end = following[0] - (following[1] == 1)
                for line in range(segment[0], end + 1):
                    key = (source["filename"], line)
                    lines[key] = lines.get(key, False) or segment[2] > 0
            for branch in source.get("branches", []):
                # LLVM emits the same source region for the DSO, unit-test crate,
                # and generic callback instances. Each source arm must run, but
                # it need not run in every codegen instance of that same region.
                key = (source["filename"], *branch[:4])
                arms = branches.setdefault(key, [False, False])
                for arm, count in enumerate(branch[4:6]):
                    assert count >= 0, f"invalid branch count: {key}"
                    arms[arm] |= count > 0
    assert lines, "missing lines instrumentation"
    missing = [key for key, covered in lines.items() if not covered]
    assert not missing, f"production lines coverage below 100%: {missing}"
    assert branches, "missing branches instrumentation"
    missing = [key for key, arms in branches.items() if not all(arms)]
    assert not missing, f"production branch coverage below 100%: {missing}"


def production_c(root: Path) -> set[Path]:
    """Return production C and header paths, excluding test fixtures."""
    paths: set[Path] = set()
    for directory in (root / "src", root / "module"):
        if not directory.exists():
            continue
        paths.update(
            path.relative_to(root)
            for suffix in ("*.c", "*.h")
            for path in directory.rglob(suffix)
        )
    return paths


def violations(root: Path) -> list[str]:
    """Return deterministic Rust-product-surface violations."""
    errors: list[str] = []
    found = production_c(root)
    if found != {LOADER}:
        rendered = ", ".join(str(path) for path in sorted(found)) or "none"
        errors.append(f"production C surface must be only {LOADER}: {rendered}")

    makefile = (root / "Makefile").read_text(encoding="utf-8")
    for token in FORBIDDEN_MAKE_TOKENS:
        if token in makefile:
            errors.append(f"Makefile retains obsolete product token: {token}")

    doxyfile = (root / "Doxyfile").read_text(encoding="utf-8")
    if "INPUT = module/app_rpt_advanced_loader.c" not in doxyfile:
        errors.append("Doxygen input is not limited to the C loader")
    return errors


def check(root: Path) -> None:
    """Assert that one source tree exposes only the intended Rust product."""
    errors = violations(root)
    assert not errors, "\n".join(errors)


def write(root: Path, relative: str, contents: str = "") -> None:
    """Write one synthetic fixture path."""
    destination = root / relative
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(contents, encoding="utf-8")


def verify_policy() -> None:
    """Exercise the accepted surface and each rejected legacy category."""
    with tempfile.TemporaryDirectory(prefix="rpt-advanced-rust-surface-") as temporary:
        root = Path(temporary)
        write(root, str(LOADER))
        write(root, "Makefile", "all:\n\t@true\n")
        write(root, "Doxyfile", f"INPUT = {LOADER.as_posix()}\n")
        check(root)

        write(root, "src/controller.c")
        assert any("production C surface" in error for error in violations(root))
        (root / "src/controller.c").unlink()

        write(root, "module/runtime.h")
        assert any("production C surface" in error for error in violations(root))
        (root / "module/runtime.h").unlink()

        for token in FORBIDDEN_MAKE_TOKENS:
            write(root, "Makefile", f"all:\n\t@echo {token}\n")
            assert any(token in error for error in violations(root))
        write(root, "Makefile", "all:\n\t@true\n")

        write(root, "Doxyfile", "INPUT = src module\n")
        assert any("Doxygen input" in error for error in violations(root))


def verify_artifact_policy() -> None:
    """Prove missing/incorrect ABI linkage, static duplication and coverage fail closed."""
    with tempfile.TemporaryDirectory(prefix="rpt-advanced-elf-policy-") as temporary:
        root = Path(temporary)
        tables = {}
        for name, dependencies in ELF_DEPENDENCIES.items():
            write(root, name)
            tables[name] = {"NEEDED": set(dependencies)}
            if name in LIBRARIES:
                tables[name]["SONAME"] = {name}
        tables["app_rpt_advanced.so"]["RUNPATH"] = {"$ORIGIN/../../rpt_advanced"}

        def read_table(path: Path, tag: str) -> set[str]:
            return tables[path.name].get(tag, set())

        def rejected() -> None:
            try:
                artifacts(root)
            except AssertionError:
                return
            raise AssertionError("invalid artifact composition was accepted")

        def inspect(*arguments: str) -> str:
            if arguments[:3] == ("nm", "-D", "--defined-only"):
                return f"0000 T {EXPORTS[Path(arguments[-1]).name]}\n"
            return ""

        with (
            patch(f"{__name__}.dynamic", side_effect=read_table),
            patch(f"{__name__}.command", side_effect=inspect) as symbols,
        ):
            artifacts(root)
            tables["app_rpt_advanced.so"]["RUNPATH"] = {
                "$ORIGIN/../../test-linux-gnu/rpt_advanced"
            }
            artifacts(root, "$ORIGIN/../../test-linux-gnu/rpt_advanced")
            rejected()
            tables["app_rpt_advanced.so"]["RUNPATH"] = {"$ORIGIN/../../rpt_advanced"}
            for name, table in tables.items():
                for tag in table:
                    previous = table[tag]
                    table[tag] = {"wrong.so"}
                    rejected()
                    table[tag] = previous
                (root / name).unlink()
                rejected()
                write(root, name)
            for duplicate in (
                "0000 T rpcr2_descriptor\n",
                "rpt_advanced_core::controller",
            ):
                symbols.side_effect = lambda *arguments, value=duplicate: (
                    value
                    if arguments[:2] == ("nm", "--defined-only")
                    else inspect(*arguments)
                )
                rejected()
            symbols.side_effect = inspect

        report = root / "coverage.json"
        for count, covered, accepted in ((1, 1, True), (0, 0, False), (2, 1, False)):
            metric = {"count": count, "covered": covered}
            write(
                root,
                report.name,
                json.dumps(
                    {
                        "data": [
                            {
                                "totals": {
                                    "lines": metric,
                                    "branches": metric,
                                },
                                "files": [
                                    {
                                        "filename": "production.rs",
                                        "branches": [[1, 1, 1, 9, 1, 1, 0, 0, 4]],
                                        "segments": [
                                            [1, 1, 1, True, True, False],
                                            [covered + 1, 1, 0, True, True, False],
                                            [count + 1, 1, 0, False, False, False],
                                        ]
                                        if count
                                        else [],
                                    }
                                ],
                            }
                        ]
                    }
                ),
            )
            try:
                coverage(report)
            except AssertionError:
                assert not accepted
            else:
                assert accepted

        for sources, accepted in (
            ([], False),
            ([{"filename": "a.rs", "branches": []}], False),
            (
                [
                    {
                        "filename": "a.rs",
                        "branches": [[1, 1, 1, 9, 3, 0], [1, 1, 1, 9, 0, 2]],
                    }
                ],
                True,
            ),
            (
                [
                    {
                        "filename": "a.rs",
                        "branches": [[1, 1, 1, 9, 3, 0], [1, 1, 1, 9, 2, 0]],
                    }
                ],
                False,
            ),
            (
                [
                    {
                        "filename": "a.rs",
                        "branches": [[1, 1, 1, 9, 3, 0], [2, 1, 2, 9, 0, 2]],
                    }
                ],
                False,
            ),
            (
                [
                    {"filename": "a.rs", "branches": [[1, 1, 1, 9, 3, 0]]},
                    {"filename": "b.rs", "branches": [[1, 1, 1, 9, 0, 2]]},
                ],
                False,
            ),
        ):
            for source in sources:
                source["segments"] = [
                    [1, 1, 1, True, True, False],
                    [2, 1, 0, False, False, False],
                ]
            write(
                root,
                report.name,
                json.dumps(
                    {
                        "data": [
                            {
                                "totals": {"lines": {"count": 1, "covered": 1}},
                                "files": sources,
                            }
                        ]
                    }
                ),
            )
            try:
                coverage(report)
            except AssertionError:
                assert not accepted
            else:
                assert accepted

        def source(name: str, segments: list) -> dict:
            return {
                "filename": name,
                "segments": segments,
                "branches": [[1, 1, 1, 9, 1, 1]],
            }

        end = [3, 1, 0, False, False, False]
        first = [[1, 1, 1, True, True, False], [2, 1, 0, True, True, False], end]
        second = [[1, 1, 0, True, True, False], [2, 1, 1, True, True, False], end]
        gap = [[1, 1, 1, True, True, True], end]
        for sources, accepted in (
            ([source("a.rs", first), source("a.rs", second)], True),
            ([source("a.rs", first), source("a.rs", first)], False),
            ([source("a.rs", first), source("b.rs", second)], False),
            ([source("a.rs", first), source("a.rs", gap)], False),
            ([source("a.rs", gap)], False),
            ([source("a.rs", [])], False),
            ([source("a.rs", [[1, 1, 1, False, False, False], end])], False),
            (
                [
                    source(
                        "a.rs",
                        [
                            [1, 1, 0, True, True, False],
                            [1, 4, 1, True, True, False],
                            [2, 1, 0, False, False, False],
                        ],
                    )
                ],
                True,
            ),
        ):
            write(root, report.name, json.dumps({"data": [{"files": sources}]}))
            try:
                coverage(report)
            except AssertionError:
                assert not accepted
            else:
                assert accepted


def verify_stage_policy() -> None:
    """Prove that obsolete installed archives/headers cannot pass the exact manifest."""
    with tempfile.TemporaryDirectory(prefix="rpt-advanced-stage-policy-") as temporary:
        root = Path(temporary)
        stage = root / "stage"
        library = "usr/lib/test-linux-gnu/rpt_advanced"
        for name in LIBRARIES:
            write(root, f"build/{name}")
            write(stage, f"{library}/{name}")
        write(root, "build/app_rpt_advanced.so")
        write(stage, "usr/lib/test-linux-gnu/asterisk/modules/app_rpt_advanced.so")
        write(root, "rust/product/include/rptadv_product.h")
        write(stage, "usr/include/rptadv_product.h")
        (stage / library / "librptadv_product.so").symlink_to(PRODUCT)
        for name, crate in (("control_asterisk", "control-asterisk-adapter"),):
            header = f"rptadv_{name}_adapter.h"
            write(root, f"rust/{crate}/include/{header}")
            write(stage, f"usr/include/{header}")
            (stage / library / f"librptadv_{name}_adapter.so").symlink_to(
                f"librptadv_{name}_adapter.so.1"
            )
        for name, crate in (("file", "file-adapter"), ("speech", "speech-adapter")):
            header = f"rptadv_{name}_adapter.h"
            write(root, f"rust/{crate}/include/{header}")
            write(root, "rust/media-support/include/rptadv_media_types.h")
            write(stage, f"usr/include/rpt_advanced/{name}/{header}")
            write(stage, f"usr/include/rpt_advanced/{name}/rptadv_media_types.h")
            (stage / library / f"librptadv_{name}_adapter.so").symlink_to(
                f"librptadv_{name}_adapter.so.1"
            )
        for source, destination in (
            *((path, path) for path in MANUALS),
            ("COPYING", "copyright"),
            ("examples/rpt_advanced.conf", "examples/rpt_advanced.conf"),
        ):
            write(root, source)
            write(stage, f"usr/share/doc/rpt-advanced/{destination}")
        resolved = "\n".join(
            f"{name} => {stage / library / name}" for name in LIBRARIES
        )
        with patch(f"{__name__}.command", return_value=resolved):
            staged(root, stage, "test-linux-gnu")
            for document in (
                "doc/allstarlink-status.md",
                "doc/architecture/decisions/0040-initial-alpha-compatibility-policy.md",
            ):
                installed = stage / "usr/share/doc/rpt-advanced" / document
                installed.unlink()
                try:
                    staged(root, stage, "test-linux-gnu")
                except FileNotFoundError:
                    pass
                else:
                    raise AssertionError(f"missing user document accepted: {document}")
                write(stage, str(installed.relative_to(stage)))
            for obsolete in (
                "usr/include/rpt_advanced/controller.h",
                "usr/lib/librpt_advanced.a",
            ):
                write(stage, obsolete)
                try:
                    staged(root, stage, "test-linux-gnu")
                except AssertionError as error:
                    assert "extra=" in str(error)
                else:
                    raise AssertionError(
                        f"obsolete installed file accepted: {obsolete}"
                    )
                (stage / obsolete).unlink()
            alternate = stage / "usr/lib/asterisk/modules/app_rpt_advanced.so"
            alternate.parent.mkdir(parents=True)
            (
                stage / "usr/lib/test-linux-gnu/asterisk/modules/app_rpt_advanced.so"
            ).rename(alternate)
            staged(root, stage, "test-linux-gnu", Path("/usr/lib/asterisk/modules"))


def main() -> None:
    """Self-test policy and validate the requested source or built product surface."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=Path)
    parser.add_argument("--stage", type=Path)
    parser.add_argument("--multiarch")
    parser.add_argument("--asteriskmoddir", type=Path)
    parser.add_argument("--libdir", type=Path)
    parser.add_argument("--rustdoc", type=Path)
    parser.add_argument("--package", type=Path)
    parser.add_argument("--coverage", type=Path)
    args = parser.parse_args()
    verify_policy()
    verify_artifact_policy()
    verify_stage_policy()
    root = Path(__file__).resolve().parents[1]
    if args.artifacts:
        if args.asteriskmoddir is not None:
            assert args.libdir is not None, "configured artifacts require --libdir"
            artifacts(
                args.artifacts,
                "$ORIGIN/"
                + os.path.relpath(args.libdir.resolve(), args.asteriskmoddir.resolve()),
            )
        else:
            artifacts(args.artifacts)
    elif args.stage:
        assert args.multiarch, "staged manifest requires --multiarch"
        staged(root, args.stage, args.multiarch, args.asteriskmoddir)
    elif args.rustdoc:
        for crate in (
            "rpt_advanced_core",
            "rptadv_product",
            "rptadv_asterisk_adapter",
            "rptadv_control_asterisk_adapter",
            "rptadv_file_adapter",
            "rptadv_speech_adapter",
        ):
            assert (args.rustdoc / crate / "index.html").is_file(), (
                f"missing Rustdoc: {crate}"
            )
    elif args.package:
        package(args.package)
    elif args.coverage:
        coverage(args.coverage)
    else:
        check(root)
    print("Rust product-surface checks passed")


if __name__ == "__main__":
    main()
