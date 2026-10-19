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
        "libusbradioplus-product1",
        "librptadv-portaudio-alsa-adapter2",
        "librptadv-gpio-adapter1",
        "librptadv-ffmpeg-adapter1",
        "librptadv-iax2-client1",
        "librptadv-resolver-adapter1",
        "librptadv-jitter-adapter1",
    ):
        assert package in runtime


def test_radio_product_dependency_does_not_require_the_asterisk_integration() -> None:
    stanzas = package_stanzas()
    source = (ROOT / "debian/control").read_text().split("\n\n", 1)[0]
    assert "libusbradioplus-product-dev (>= 0.1.0~alpha24-1.deb13)" in source
    standalone = depends(stanzas["rpt-advanced"])
    assert "libusbradioplus-product1 (>= 0.1.0~alpha24-1.deb13)" in standalone
    assert "usbradioplus" not in standalone.replace("libusbradioplus-product1", "")
    for package in ("app-rpt-advanced", "librptadv-product1"):
        assert "usbradioplus" not in depends(stanzas[package])
        assert "librptadv-resolver-adapter1" not in depends(stanzas[package])
        assert "librptadv-jitter-adapter1" not in depends(stanzas[package])


def test_jitter_runtime_and_development_manifests() -> None:
    stanzas = package_stanzas()
    runtime = stanzas["librptadv-jitter-adapter1"]
    assert "${shlibs:depends}" in depends(runtime)
    assert "librptadv-jitter-adapter1 (= ${binary:version})" in depends(
        stanzas["librptadv-jitter-adapter-dev"]
    )
    assert (
        ROOT / "debian/librptadv-jitter-adapter1.install"
    ).read_text().splitlines() == [
        "usr/lib/*/rpt_advanced/librptadv_jitter_adapter.so.1*"
    ]
    assert set(
        (ROOT / "debian/librptadv-jitter-adapter-dev.install").read_text().splitlines()
    ) == {
        "usr/include/rpt_advanced/jitter/rptadv_jitter_adapter.h",
        "usr/lib/*/rpt_advanced/librptadv_jitter_adapter.so",
        "usr/lib/*/pkgconfig/rptadv_jitter_adapter.pc",
    }


def test_resolver_runtime_and_development_manifests() -> None:
    stanzas = package_stanzas()
    assert "librptadv-resolver-adapter1 (= ${binary:version})" in depends(
        stanzas["librptadv-resolver-adapter-dev"]
    )
    assert (
        ROOT / "debian/librptadv-resolver-adapter1.install"
    ).read_text().splitlines() == [
        "usr/lib/*/rpt_advanced/librptadv_resolver_adapter.so.1*"
    ]
    assert set(
        (ROOT / "debian/librptadv-resolver-adapter-dev.install")
        .read_text()
        .splitlines()
    ) == {
        "usr/include/rpt_advanced/resolver/rptadv_resolver_adapter.h",
        "usr/lib/*/rpt_advanced/librptadv_resolver_adapter.so",
    }


def test_standalone_package_declares_its_service_account_provisioner() -> None:
    runtime = depends(package_stanzas()["rpt-advanced"])
    postinst = (ROOT / "debian/rpt-advanced.postinst").read_text()

    assert "adduser" in runtime
    assert "adduser --system --group" in postinst


def test_site_gpio_packages_are_optional_and_separately_installable() -> None:
    stanzas = package_stanzas()
    runtime = "librptadv-gpiod-adapter1"
    assert "${shlibs:depends}" in depends(stanzas[runtime])
    assert f"{runtime} (= ${{binary:version}})" in depends(
        stanzas["librptadv-gpiod-adapter-dev"]
    )
    for entry in ("rpt-advanced", "app-rpt-advanced", "librptadv-product1"):
        assert runtime not in depends(stanzas[entry])
    assert (ROOT / f"debian/{runtime}.install").read_text().splitlines() == [
        "usr/lib/*/rpt_advanced/librptadv_gpiod_adapter.so.1*"
    ]
    assert set(
        (ROOT / "debian/librptadv-gpiod-adapter-dev.install").read_text().splitlines()
    ) == {
        "usr/include/rpt_advanced/gpiod/rptadv_gpiod_adapter.h",
        "usr/lib/*/rpt_advanced/librptadv_gpiod_adapter.so",
    }


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
        test_radio_product_dependency_does_not_require_the_asterisk_integration,
        test_standalone_package_declares_its_service_account_provisioner,
        test_standalone_build_profile_excludes_asterisk_adapter_packages,
        test_resolver_runtime_and_development_manifests,
        test_jitter_runtime_and_development_manifests,
        test_site_gpio_packages_are_optional_and_separately_installable,
    )
    for test in tests:
        test()
    print(f"{len(tests)} standalone package-combination checks passed")
