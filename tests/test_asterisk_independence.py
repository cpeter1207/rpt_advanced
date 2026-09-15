#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""! @file
@brief Enforce ADR 0036's source-level Asterisk and ASL3 dependency boundary.

The check deliberately examines only C include directives, module-descriptor
dependencies, and code tokens.  It permits public Asterisk APIs and
AllStarLink wire-protocol strings while preventing portable source from
acquiring a host dependency or the module from importing known ASL3 internals.
"""

from __future__ import annotations

import re
import tempfile
from pathlib import Path

## @brief Matches one direct C preprocessor include path.
INCLUDE = re.compile(r'^\s*#\s*include\s*[<"]([^">]+)[">]', re.MULTILINE)
## @brief Matches host-framework header path components forbidden to portable source.
FORBIDDEN_PORTABLE_HEADER = re.compile(r"(?:^|/)(?:asterisk|asl3?)(?:/|\.h$)")
## @brief Matches known ASL-private header spellings forbidden to module source.
FORBIDDEN_MODULE_HEADER = re.compile(
    r"(?:^|/)(?:app_rpt\.h|res_usbradio\.h|"
    r"chan_(?:usbradio|simpleusb|voter|echolink|irlp)\.h|"
    r"(?:rpt|xpmr|simpleusb|usbradio|voter|echolink)\.h)$"
)
## @brief Matches direct ASL radio helper code tokens outside literals/comments.
FORBIDDEN_RADIO_SYMBOL = re.compile(r"\bast_radio_[A-Za-z0-9_]*\b")
## @brief Extracts declared optional or required Asterisk module names.
MODULE_DEPENDENCY = re.compile(r'\.(?:optional|required)_modules\s*=\s*"([^"]*)"')
## @brief Matches the ASL module names prohibited from the app-module descriptor.
FORBIDDEN_MODULE_DEPENDENCY = re.compile(
    r"^(?:app_rpt|res_usbradio|chan_(?:usbradio|simpleusb|voter|echolink|irlp))(?:\.so)?$|^asl3(?:_|$)"
)
## @brief Removes C comments and quoted literals before a code-token scan.
NON_CODE = re.compile(
    r"/\*.*?\*/|//[^\n]*|\"(?:\\.|[^\"\\])*\"|'(?:\\.|[^'\\])*'", re.DOTALL
)
## @brief Removes C comments while retaining quoted include paths.
COMMENTS = re.compile(r"/\*.*?\*/|//[^\n]*", re.DOTALL)


def c_sources(directory: Path) -> list[Path]:
    """! @brief Return the deterministic C/header scan set below one source directory.
    @param directory Existing checked source directory.
    @return Sorted C and header paths.
    """
    return sorted((*directory.rglob("*.c"), *directory.rglob("*.h")))


def include_paths(source: Path) -> list[str]:
    """! @brief Extract direct header names after excluding C comments.
    @param source C or header source file.
    @return Include path spelling from each preprocessor include directive.
    """
    return INCLUDE.findall(COMMENTS.sub("", source.read_text(encoding="utf-8")))


def code_only(source: Path) -> str:
    """! @brief Remove C comments and quoted literals before a symbol-token scan.
    @param source C or header source file.
    @return Remaining source text, sufficient for this exact token policy.
    """
    return NON_CODE.sub("", source.read_text(encoding="utf-8"))


def check_portable_source(root: Path) -> list[str]:
    """! @brief Reject host/ASL header imports from portable controller source.
    @param root Synthetic or repository root containing @c src.
    @return Complete deterministic violation descriptions.
    """
    violations: list[str] = []
    for source in c_sources(root / "src"):
        for header in include_paths(source):
            if FORBIDDEN_PORTABLE_HEADER.search(
                header
            ) or FORBIDDEN_MODULE_HEADER.search(header):
                violations.append(
                    f"{source.relative_to(root)} imports host header {header}"
                )
    return violations


def check_module_source(root: Path) -> list[str]:
    """! @brief Reject direct ASL-private dependencies from the Asterisk adapter source.
    @param root Synthetic or repository root containing @c module.
    @return Complete deterministic violation descriptions.
    """
    violations: list[str] = []
    for source in c_sources(root / "module"):
        for header in include_paths(source):
            if FORBIDDEN_MODULE_HEADER.search(header):
                violations.append(
                    f"{source.relative_to(root)} imports ASL-private header {header}"
                )
        if FORBIDDEN_RADIO_SYMBOL.search(code_only(source)):
            violations.append(f"{source.relative_to(root)} uses ast_radio_* helper")
    descriptor = root / "module" / "app_rpt_advanced.c"
    for dependency in MODULE_DEPENDENCY.findall(descriptor.read_text(encoding="utf-8")):
        for name in dependency.split(","):
            if FORBIDDEN_MODULE_DEPENDENCY.search(name.strip()):
                violations.append(
                    f"{descriptor.relative_to(root)} requires ASL module {name.strip()}"
                )
    return violations


def check_tree(root: Path) -> None:
    """! @brief Assert that one source tree satisfies the checked ADR 0036 boundaries.
    @param root Repository or synthetic root to check.
    @return None; raises only for a boundary violation.
    @exception AssertionError When one direct forbidden dependency is found.
    """
    violations = check_portable_source(root) + check_module_source(root)
    assert not violations, "\n".join(violations)


def write(root: Path, relative: str, contents: str) -> None:
    """! @brief Create one UTF-8 synthetic source fixture.
    @param root Temporary fixture root.
    @param relative Path below @p root.
    @param contents Complete file contents.
    @return None after the fixture file is written.
    """
    destination = root / relative
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(contents, encoding="utf-8")


def expect_violation(root: Path, expected: str) -> None:
    """! @brief Assert that a synthetic tree rejects one intended dependency.
    @param root Temporary fixture root.
    @param expected Distinct expected diagnostic fragment.
    @return None after the expected rejection is observed.
    """
    try:
        check_tree(root)
    except AssertionError as error:
        assert expected in str(error), error
    else:
        raise AssertionError(f"expected {expected}")


def verify_examples() -> None:
    """! @brief Cover accepted public-Asterisk/wire cases and every forbidden category.

    The accepted fixture intentionally uses @c AST_CONTROL_RADIO_KEY and an
    @c app_rpt wire-protocol string, proving that the policy is not a broad
    token ban. Each rejection fixture changes one checked category only.
    @return None after every representative policy case is checked.
    """
    with tempfile.TemporaryDirectory(prefix="rpt-advanced-adr0036-") as temporary:
        root = Path(temporary)
        write(
            root,
            "src/controller.c",
            '/* #include <asterisk/channel.h> */\n#include "controller.h"\n',
        )
        write(
            root,
            "module/adapter.c",
            '#include "app_rpt_advanced.h"\n'
            '#include "chan_usbradioplus.h"\n'
            "#include <asterisk/channel.h>\n"
            'const char *wire = "app_rpt L";\n'
            "int key = AST_CONTROL_RADIO_KEY;\n",
        )
        write(
            root,
            "module/app_rpt_advanced.c",
            "#include <asterisk/module.h>\n"
            'struct descriptor value = {.optional_modules = "chan_usbradioplus"};\n',
        )
        check_tree(root)

        write(root, "src/controller.c", "#include <asterisk/channel.h>\n")
        expect_violation(root, "imports host header")
        write(root, "src/controller.c", "#include <asterisk.h>\n")
        expect_violation(root, "imports host header")
        write(root, "src/controller.c", '#include "app_rpt.h"\n')
        expect_violation(root, "imports host header")
        write(root, "src/controller.c", '#include "controller.h"\n')

        write(root, "module/adapter.c", '#include "res_usbradio.h"\n')
        expect_violation(root, "imports ASL-private header")
        write(root, "module/adapter.c", "int value = ast_radio_get_state();\n")
        expect_violation(root, "uses ast_radio_*")
        write(root, "module/adapter.c", "#include <asterisk/channel.h>\n")

        write(
            root,
            "module/app_rpt_advanced.c",
            'struct descriptor value = {.optional_modules = "res_usbradio"};\n',
        )
        expect_violation(root, "requires ASL module")


def main() -> None:
    """! @brief Run self-tests before checking the actual repository source tree.
    @return None; an assertion reports a boundary violation.
    """
    verify_examples()
    check_tree(Path(__file__).resolve().parents[1])
    print("ADR 0036 Asterisk/ASL3 source-boundary checks passed")


if __name__ == "__main__":
    main()
