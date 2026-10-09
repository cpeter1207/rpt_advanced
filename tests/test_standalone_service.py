#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Check standalone service isolation and package boundaries."""

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def package_stanza(name: str) -> str:
    control = (ROOT / "debian/control").read_text(encoding="utf-8")
    for stanza in control.split("\n\n"):
        if stanza.splitlines()[0].startswith(f"Package: {name}"):
            return stanza
    raise AssertionError(f"missing Debian package stanza: {name}")


def test_standalone_service_runs_as_dedicated_unprivileged_user() -> None:
    service = (ROOT / "debian/rpt-advanced.service").read_text(encoding="utf-8")
    assert "User=rpt-advanced" in service
    assert "Group=rpt-advanced" in service
    assert (
        "ExecStart=/usr/bin/rpt-advanced --foreground /etc/rpt_advanced/rpt_advanced.conf"
        in service
    )
    assert "Restart=on-failure" in service
    assert "NoNewPrivileges=yes" in service
    assert "ProtectSystem=strict" in service


def test_standalone_package_installs_the_service_configuration_as_a_conffile() -> None:
    manifest = (ROOT / "debian/rpt-advanced.install").read_text(encoding="utf-8")
    assert (
        "usr/share/doc/rpt-advanced/examples/rpt_advanced.conf etc/rpt_advanced"
        in manifest.splitlines()
    )


def test_debian_install_manifests_are_not_executable_scripts() -> None:
    for name in ("rpt-advanced.install", "app-rpt-advanced.install"):
        mode = (ROOT / "debian" / name).stat().st_mode
        assert mode & 0o111 == 0, f"debian/{name} must be parsed as a manifest"


def test_standalone_package_does_not_depend_on_asterisk() -> None:
    stanza = package_stanza("rpt-advanced")
    depends = stanza.split("Depends:", 1)[1].split("Description:", 1)[0]
    assert "asterisk" not in depends.lower()
    assert "app-rpt-advanced" not in depends.lower()
    assert "librptadv-iax2-client1 (>= 0.1.0~alpha4)" in depends


def test_standalone_requires_radio_core_with_live_update_api() -> None:
    control = (ROOT / "debian/control").read_text(encoding="utf-8")
    assert "librptadvradio-dev (>= 0.1.0~alpha6)" in control
    assert "librptadvradio4 (>= 0.1.0~alpha6)" in package_stanza("rpt-advanced")


def test_released_iax2_library_is_not_rebuilt_as_a_candidate_dependency() -> None:
    manifest = (ROOT / ".github/dependencies.json").read_text(encoding="utf-8")
    dependencies = json.loads(manifest)
    assert all(item["repository"] != "librptadviax2" for item in dependencies)


def test_standalone_package_grants_service_user_cm119_usb_access() -> None:
    rules = (ROOT / "debian/rpt-advanced.udev").read_text(encoding="utf-8")
    assert 'ATTR{idVendor}=="0d8c"' in rules
    assert 'ATTR{idProduct}=="013c"' in rules
    assert 'GROUP="rpt-advanced"' in rules
    assert 'MODE="0660"' in rules

    postinst = (ROOT / "debian/rpt-advanced.postinst").read_text(encoding="utf-8")
    assert "udevadm control --reload-rules" in postinst
    assert "udevadm trigger --action=change --subsystem-match=usb" in postinst
    assert "udevadm settle --timeout=10" in postinst


def test_asterisk_adapter_is_separately_installable() -> None:
    stanza = package_stanza("app-rpt-advanced")
    assert "${asterisk:Depends}" in stanza
    assert "usr/lib/*/asterisk/modules/app_rpt_advanced.so" in (
        ROOT / "debian/app-rpt-advanced.install"
    ).read_text(encoding="utf-8")


def test_standalone_build_profile_omits_asterisk_build_and_adapter() -> None:
    control = (ROOT / "debian/control").read_text(encoding="utf-8")
    rules = (ROOT / "debian/rules").read_text(encoding="utf-8")
    assert "dh-sequence-asterisk <!standalone>" in control
    assert "filter standalone,$(DEB_BUILD_PROFILES)" in rules
    assert "-Napp-rpt-advanced" in rules
    assert "-Nlibrptadv-control-asterisk-adapter1" in rules
    assert "standalone-build" in rules
    assert "standalone-install" in rules


def test_shlibdeps_searches_every_private_adapter_library_directory() -> None:
    rules = (ROOT / "debian/rules").read_text(encoding="utf-8")
    assignment = next(
        line for line in rules.splitlines() if line.startswith("PRIVATE_LIB_DIRS :=")
    )
    for package in (
        "librptadv-product1",
        "librptadv-file-adapter1",
        "librptadv-speech-adapter1",
        "librptadv-control-asterisk-adapter1",
        "librptadv-control-standalone-adapter1",
    ):
        assert (
            f"debian/{package}/usr/lib/$(DEB_HOST_MULTIARCH)/rpt_advanced" in assignment
        )


if __name__ == "__main__":
    for test in (
        test_standalone_service_runs_as_dedicated_unprivileged_user,
        test_standalone_package_installs_the_service_configuration_as_a_conffile,
        test_debian_install_manifests_are_not_executable_scripts,
        test_standalone_package_does_not_depend_on_asterisk,
        test_standalone_requires_radio_core_with_live_update_api,
        test_released_iax2_library_is_not_rebuilt_as_a_candidate_dependency,
        test_standalone_package_grants_service_user_cm119_usb_access,
        test_asterisk_adapter_is_separately_installable,
        test_standalone_build_profile_omits_asterisk_build_and_adapter,
        test_shlibdeps_searches_every_private_adapter_library_directory,
    ):
        test()
    print("standalone service and package boundaries valid")
