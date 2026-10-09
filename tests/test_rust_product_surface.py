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
STANDALONE_CONTROL = "librptadv_control_standalone_adapter.so.1"
STANDALONE_BINARY = "rpt-advanced"
PRODUCT = "librptadv_product.so.1"
RING_MINIMUM_VERSION = "3.0.0~alpha2"
SAMPLERATE_MINIMUM_VERSION = "0.2.0~alpha1"
LIBRARIES = {PRODUCT, *(f"librptadv_{name}_adapter.so.1" for name in ADAPTERS)}
ELF_DEPENDENCIES = {
    "app_rpt_advanced.so": LIBRARIES,
    "librptadv_asterisk_adapter.so.1": set(),
    PRODUCT: {
        "librate_adjusting_pcm_ring3.so.3",
        "librptadv_samplerate_adapter.so.2",
    },
    "librptadv_control_asterisk_adapter.so.1": set(),
    "librptadv_file_adapter.so.1": set(),
    "librptadv_speech_adapter.so.1": set(),
    STANDALONE_CONTROL: set(),
}
EXPORTS = {
    "librptadv_asterisk_adapter.so.1": "rptadv_asterisk_descriptor_v1",
    PRODUCT: "rptadv_product_descriptor_v1",
    "librptadv_control_asterisk_adapter.so.1": "rptadv_control_descriptor_v1",
    "librptadv_file_adapter.so.1": "rptadv_file_adapter_descriptor",
    "librptadv_speech_adapter.so.1": "rptadv_speech_adapter_descriptor",
    STANDALONE_CONTROL: "rptadv_control_standalone_descriptor_v1",
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
        if name in LIBRARIES or name == STANDALONE_CONTROL:
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
            r"\b(rpcr3_descriptor|rptadv_samplerate_adapter_descriptor)$",
            symbols,
            re.MULTILINE,
        ), f"static external adapter in {name}"
        if name != PRODUCT:
            assert "rpt_advanced_core::" not in symbols, f"duplicated core in {name}"
    assert dynamic(directory / "app_rpt_advanced.so", "RUNPATH") == {runpath}, (
        "loader must resolve the configured private-library directory"
    )
    standalone = directory / STANDALONE_BINARY
    assert standalone.is_file(), f"missing artifact: {standalone}"
    needed = dynamic(standalone, "NEEDED")
    assert not any(
        "asterisk" in name.lower() or "asl3" in name.lower() for name in needed
    ), f"standalone executable has an Asterisk/ASL3 dependency: {needed}"


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
    expected = {
        module,
        Path("usr/bin") / STANDALONE_BINARY,
        *(library / name for name in LIBRARIES),
        library / STANDALONE_CONTROL,
    }
    link = library / "librptadv_product.so"
    expected.add(link)
    assert (stage / link).is_symlink(), f"missing developer link: {link}"
    assert (stage / link).readlink() == Path(PRODUCT)
    link = library / "librptadv_control_standalone_adapter.so"
    expected.add(link)
    assert (stage / link).is_symlink(), f"missing developer link: {link}"
    assert (stage / link).readlink() == Path(STANDALONE_CONTROL)
    header = Path("usr/include/rptadv_product.h")
    expected.add(header)
    assert (stage / header).read_bytes() == (
        root / "rust/product/include/rptadv_product.h"
    ).read_bytes()
    header = Path("usr/include/rptadv_control_adapter.h")
    expected.add(header)
    assert (stage / header).read_bytes() == (
        root / "rust/control-abi/include/rptadv_control_adapter.h"
    ).read_bytes()
    header = Path("usr/include/rptadv_control_standalone_adapter.h")
    expected.add(header)
    assert (stage / header).read_bytes() == (
        root
        / "rust/control-standalone-adapter/include/rptadv_control_standalone_adapter.h"
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
    catalog = Path("usr/share/asterisk/rpt_advanced/messages/en-US.ftl")
    expected.add(catalog)
    assert (stage / catalog).read_bytes() == (root / "messages/en-US.ftl").read_bytes()
    standalone_catalog = Path("usr/share/rpt-advanced/messages/en-US.ftl")
    expected.add(standalone_catalog)
    assert (stage / standalone_catalog).read_bytes() == (
        root / "messages/en-US.ftl"
    ).read_bytes()
    found = {
        path.relative_to(stage)
        for path in stage.rglob("*")
        if path.is_file() or path.is_symlink()
    }
    assert found == expected, (
        f"staged manifest: missing={expected - found}, extra={found - expected}"
    )
    for name in (*LIBRARIES, STANDALONE_CONTROL):
        assert (stage / library / name).read_bytes() == (
            root / "build" / name
        ).read_bytes()
    assert (stage / "usr/bin" / STANDALONE_BINARY).read_bytes() == (
        standalone_binary(root)
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


def standalone_binary(root: Path) -> Path:
    """Resolve the executable from Cargo's selected build directory."""
    target = Path(os.environ.get("CARGO_TARGET_DIR", "target"))
    if not target.is_absolute():
        target = root / target
    return target / "release" / STANDALONE_BINARY


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
        "librate-adjusting-pcm-ring3-dev",
        "librptadv-samplerate-adapter-dev",
        "${shlibs:Depends}",
        "${misc:Depends}",
        "${asterisk:Depends}",
    ):
        assert token in contents, f"missing package dependency: {token}"
    for token in (
        f"librate-adjusting-pcm-ring3-dev (>= {RING_MINIMUM_VERSION})",
        f"librate-adjusting-pcm-ring3 (>= {RING_MINIMUM_VERSION})",
        f"librptadv-samplerate-adapter-dev (>= {SAMPLERATE_MINIMUM_VERSION})",
        f"librptadv-samplerate-adapter2 (>= {SAMPLERATE_MINIMUM_VERSION})",
    ):
        assert token in contents, f"missing compatible audio provider: {token}"
    stanzas = {
        stanza.splitlines()[0].removeprefix("Package: ").split()[0]: stanza
        for stanza in contents.split("\n\n")
        if stanza.startswith("Package: ")
    }
    standalone = stanzas["rpt-advanced"].split("Description:", 1)[0]
    adapter = stanzas["app-rpt-advanced"].split("Description:", 1)[0]
    assert "${asterisk:Depends}" not in standalone, (
        "standalone package depends on Asterisk"
    )
    assert "${asterisk:Depends}" in adapter, "Asterisk adapter lost its dependency"
    assert "asl3-asterisk" not in contents, "ASL3-specific package dependency"


def _cfg_item_end(source: str) -> int:
    """Return the 1-based line where a cfg(test) item ends."""
    state = "code"
    block_depth = 0
    raw_end = ""
    braces = 0
    body = False
    parentheses = brackets = 0
    line = 1
    index = 0
    while index < len(source):
        char = source[index]
        following = source[index : index + 2]
        if char == "\n":
            line += 1
        if state == "line_comment":
            if char == "\n":
                state = "code"
        elif state == "block_comment":
            if following == "/*":
                block_depth += 1
                index += 1
            elif following == "*/":
                block_depth -= 1
                index += 1
                if block_depth == 0:
                    state = "code"
        elif state == "string":
            if char == "\\":
                index += 1
            elif char == '"':
                state = "code"
        elif state == "raw_string":
            if source.startswith(raw_end, index):
                index += len(raw_end) - 1
                state = "code"
        else:
            raw = re.match(r"(?:b|c)?r(#+)?\"", source[index:])
            if raw:
                raw_end = '"' + (raw.group(1) or "")
                index += len(raw.group(0)) - 1
                state = "raw_string"
            elif following == "//":
                state = "line_comment"
                index += 1
            elif following == "/*":
                state = "block_comment"
                block_depth = 1
                index += 1
            elif char == '"':
                state = "string"
            elif char == "(":
                parentheses += 1
            elif char == ")":
                parentheses -= 1
            elif char == "[":
                brackets += 1
            elif char == "]":
                brackets -= 1
            elif char == "{":
                body = True
                braces += 1
            elif char == "}" and body:
                braces -= 1
                if braces == 0:
                    return line
            elif not body and parentheses == 0 and brackets == 0 and char in ";,":
                return line
        index += 1
    return line


def get_test_only_ranges(filename: str) -> list[tuple[int, int]]:
    """Return source line ranges compiled only with Rust's test configuration."""
    try:
        lines = Path(filename).read_text(encoding="utf-8").splitlines()
    except OSError:
        return []
    ranges = []
    index = 0
    while index < len(lines):
        if not re.fullmatch(r"\s*#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*", lines[index]):
            index += 1
            continue
        start = index + 1
        item = start
        while item < len(lines) and (
            not lines[item].strip() or lines[item].lstrip().startswith("#[")
        ):
            item += 1
        if item == len(lines):
            ranges.append((start, start))
            index += 1
            continue
        end = item + _cfg_item_end("\n".join(lines[item:]))
        ranges.append((start, end))
        index = end
    return ranges


def test_separate_rust_test_source_is_not_production_coverage() -> None:
    assert is_test_source("/workspace/rust/control-standalone-adapter/src/tests.rs")
    assert is_test_source("/workspace/rust/core/src/runtime/aggregate_tests.rs")
    assert is_test_source("/workspace/rust/asterisk/src/fixture.rs")
    assert not is_test_source("/workspace/rust/core/src/runtime/aggregate.rs")


def test_cfg_test_items_are_excluded_without_hiding_following_production(
    tmp_path: Path,
) -> None:
    source = tmp_path / "mixed.rs"
    source.write_text(
        "pub fn before() {}\n"
        "#[cfg(test)]\n"
        "impl Fixture {\n"
        '    fn helper() { let text = "}"; /* { } */ }\n'
        "}\n"
        "#[cfg(test)]\n"
        "fn test_helper() {\n"
        '    let raw = r#"}"#;\n'
        "}\n"
        "pub fn after() {}\n",
        encoding="utf-8",
    )

    assert get_test_only_ranges(str(source)) == [(2, 5), (6, 9)]


def test_standalone_binary_uses_the_configured_cargo_target_directory(
    tmp_path: Path,
) -> None:
    target = tmp_path / "coverage-target"
    with patch.dict(os.environ, {"CARGO_TARGET_DIR": str(target)}):
        assert standalone_binary(tmp_path) == target / "release" / STANDALONE_BINARY


def is_test_source(filename: str) -> bool:
    """Identify separately compiled Rust test sources outside production coverage."""
    path = filename.replace("\\", "/")
    name = path.rsplit("/", 1)[-1]
    return (
        "/tests/" in path
        or name in {"fixture.rs", "tests.rs"}
        or name.endswith("_tests.rs")
    )


def coverage(report: Path) -> None:
    """Require every executable source line/branch, unioning duplicate LLVM instances."""
    data = json.loads(report.read_text(encoding="utf-8"))
    lines = {}
    branches = {}
    assert data["data"], "missing coverage instrumentation"
    for unit in data["data"]:
        for source in unit["files"]:
            if is_test_source(source["filename"]):
                continue
            segments = source.get("segments", [])
            test_ranges = get_test_only_ranges(source["filename"])
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
                    if any(start <= line <= end for start, end in test_ranges):
                        continue
                    key = (source["filename"], line)
                    lines[key] = lines.get(key, False) or segment[2] > 0
            for branch in source.get("branches", []):
                if any(start <= branch[0] <= end for start, end in test_ranges):
                    continue
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
            if name in LIBRARIES or name == STANDALONE_CONTROL:
                tables[name]["SONAME"] = {name}
        write(root, STANDALONE_BINARY)
        tables[STANDALONE_BINARY] = {"NEEDED": set()}
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
                    table[tag] = {
                        "libasterisk.so" if name == STANDALONE_BINARY else "wrong.so"
                    }
                    rejected()
                    table[tag] = previous
                (root / name).unlink()
                rejected()
                write(root, name)
            for duplicate in (
                "0000 T rpcr3_descriptor\n",
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

        source_file = root / "production.rs"
        write(
            root, source_file.name, "fn production() {}\n#[cfg(test)]\nmod tests {\n}\n"
        )
        write(
            root,
            report.name,
            json.dumps(
                {
                    "data": [
                        {
                            "files": [
                                {
                                    "filename": str(source_file),
                                    "segments": [
                                        [1, 1, 1, True, True, False],
                                        [2, 1, 0, True, True, False],
                                        [4, 1, 0, False, False, False],
                                    ],
                                    "branches": [
                                        [1, 1, 1, 10, 1, 1],
                                        [3, 1, 3, 10, 0, 0],
                                    ],
                                }
                            ]
                        }
                    ]
                }
            ),
        )
        coverage(report)

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
        for name in (*LIBRARIES, STANDALONE_CONTROL):
            write(root, f"build/{name}")
            write(stage, f"{library}/{name}")
        target_directory = "coverage-target"
        write(root, f"{target_directory}/release/{STANDALONE_BINARY}")
        write(stage, f"usr/bin/{STANDALONE_BINARY}")
        write(root, "build/app_rpt_advanced.so")
        write(stage, "usr/lib/test-linux-gnu/asterisk/modules/app_rpt_advanced.so")
        write(root, "rust/product/include/rptadv_product.h")
        write(stage, "usr/include/rptadv_product.h")
        write(root, "rust/control-abi/include/rptadv_control_adapter.h")
        write(stage, "usr/include/rptadv_control_adapter.h")
        write(
            root,
            "rust/control-standalone-adapter/include/rptadv_control_standalone_adapter.h",
        )
        write(stage, "usr/include/rptadv_control_standalone_adapter.h")
        (stage / library / "librptadv_product.so").symlink_to(PRODUCT)
        (stage / library / "librptadv_control_standalone_adapter.so").symlink_to(
            STANDALONE_CONTROL
        )
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
        write(root, "messages/en-US.ftl")
        write(stage, "usr/share/asterisk/rpt_advanced/messages/en-US.ftl")
        write(stage, "usr/share/rpt-advanced/messages/en-US.ftl")
        resolved = "\n".join(
            f"{name} => {stage / library / name}" for name in LIBRARIES
        )
        with (
            patch.dict(os.environ, {"CARGO_TARGET_DIR": target_directory}),
            patch(f"{__name__}.command", return_value=resolved),
        ):
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
            "rptadv_control_standalone_adapter",
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
