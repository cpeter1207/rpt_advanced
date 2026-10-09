#!/usr/bin/env python3
"""Check that standalone and Asterisk packages remain independently installable."""

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def package_stanzas() -> dict[str, str]:
    paragraphs = (ROOT / "debian/control").read_text().split("\n\n")
    return {
        paragraph.splitlines()[0].removeprefix("Package: "): paragraph
        for paragraph in paragraphs
        if paragraph.startswith("Package: ")
    }


def depends(stanza: str) -> str:
    lines = stanza.splitlines()
    start = next(
        index for index, line in enumerate(lines) if line.startswith("Depends:")
    )
    value = [lines[start].partition(":")[2].strip()]
    for line in lines[start + 1 :]:
        if not line.startswith((" ", "\t")):
            break
        value.append(line.strip())
    return " ".join(value).lower()


def test_standalone_package_is_independent_from_asterisk_adapter() -> None:
    stanzas = package_stanzas()
    standalone = depends(stanzas["rpt-advanced"])
    adapter = depends(stanzas["app-rpt-advanced"])

    assert "asterisk" not in standalone
    assert "app-rpt-advanced" not in standalone
    assert "rpt-advanced" not in adapter
    assert "${asterisk:depends}" in adapter


def test_each_install_set_contains_only_its_entrypoint() -> None:
    standalone = (ROOT / "debian/rpt-advanced.install").read_text().splitlines()
    adapter = (ROOT / "debian/app-rpt-advanced.install").read_text().splitlines()

    assert "usr/bin/rpt-advanced" in standalone
    assert not any(
        "asterisk/modules/app_rpt_advanced.so" in item for item in standalone
    )
    assert any("asterisk/modules/app_rpt_advanced.so" in item for item in adapter)
    assert not any(item == "usr/bin/rpt-advanced" for item in adapter)


def test_standalone_package_declares_its_native_runtime_providers() -> None:
    runtime = depends(package_stanzas()["rpt-advanced"])

    for package in (
        "librptadv-portaudio-alsa-adapter2",
        "librptadv-gpio-adapter1",
        "librptadv-ffmpeg-adapter1",
        "librptadv-iax2-client1",
    ):
        assert package in runtime


def test_standalone_package_declares_its_service_account_provisioner() -> None:
    runtime = depends(package_stanzas()["rpt-advanced"])
    postinst = (ROOT / "debian/rpt-advanced.postinst").read_text()

    assert "adduser" in runtime
    assert "adduser --system --group" in postinst


def test_standalone_build_profile_excludes_asterisk_adapter_packages() -> None:
    rules = (ROOT / "debian/rules").read_text()

    assert "filter standalone,$(DEB_BUILD_PROFILES)" in rules
    assert "-Napp-rpt-advanced" in rules
    assert "-Nlibrptadv-control-asterisk-adapter1" in rules
    assert "-Nlibrptadv-control-asterisk-adapter-dev" in rules


if __name__ == "__main__":
    tests = (
        test_standalone_package_is_independent_from_asterisk_adapter,
        test_each_install_set_contains_only_its_entrypoint,
        test_standalone_package_declares_its_native_runtime_providers,
        test_standalone_package_declares_its_service_account_provisioner,
        test_standalone_build_profile_excludes_asterisk_adapter_packages,
    )
    for test in tests:
        test()
    print(f"{len(tests)} standalone package-combination checks passed")
