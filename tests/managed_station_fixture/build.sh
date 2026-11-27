#!/bin/bash
# Invoked only inside the reviewed Native foreground qualification supervisor.
set -euo pipefail
umask 077
export LC_ALL=C

base=${1:?absolute private Native run directory}
linkdir=${2:?path to the qualified one-thread lld directory}
dev="$HOME/rpt-development"
test "$(id -u)" = 1000 && test "$(uname -m)" = aarch64
case "$base" in "$dev"/*) ;; *) exit 1 ;; esac
test "$(realpath -e "$base")" = "$base" && test -x "$linkdir/ld.lld"
src="$base/src/tests/managed_station_fixture"
out="$base/station-fixture"

# Match standalone bindgen's installed audio descriptor, including its optional tail.
audio_include=/usr/include
gpio_include="$dev/task-53-rf/arm64-1/sdk/include"
sha256sum --strict --quiet -c - <<HEADERS
3a047a1518b79b6c2e171e99e08de636b46ab7a285efb1e78ee3c3bcd7caa01d  $audio_include/rptadv_portaudio_alsa_adapter/rptadv_portaudio_alsa_adapter.h
85fa20420ae7e516cad75cc83dcf73c1eea6de7b1a27a0b890f00a27ff619b15  $gpio_include/rptadv_gpio_adapter/rptadv_gpio_adapter.h
HEADERS

mkdir -m 700 "$out"
audio=librptadv_portaudio_alsa_adapter.so.2
gpio=librptadv_gpio_adapter.so.1
common=(-std=c11 -Wall -Wextra -Werror -O2 -shared -fPIC -fvisibility=hidden
        -B"$linkdir" -fuse-ld=lld -Wl,--threads=1 -Wl,-z,defs)
cc "${common[@]}" -pthread -I"$audio_include" -Wl,-soname,"$audio" "$src/audio.c" -o "$out/$audio"
cc "${common[@]}" -I"$gpio_include" -Wl,-soname,"$gpio" "$src/gpio.c" -o "$out/$gpio"

check_elf() {
    local name=$1 getter=$2
    readelf -h "$out/$name" | grep -Eq 'Machine:[[:space:]]+AArch64'
    test "$(readelf -d "$out/$name" | sed -n 's/.*(SONAME).*\[\(.*\)\].*/\1/p')" = "$name"
    test "$(readelf -d "$out/$name" | sed -n 's/.*(NEEDED).*\[\(.*\)\].*/\1/p')" = libc.so.6
    test "$(readelf --wide --dyn-syms "$out/$name" |
        awk '($5 == "GLOBAL" || $5 == "WEAK") && $7 != "UND" {print $8}')" = "$getter"
}
check_elf "$audio" rptadv_portaudio_alsa_adapter_descriptor
check_elf "$gpio" rptadv_gpio_adapter_descriptor

python3 - "$out/$audio" "$out/$gpio" <<'PY'
import ctypes
import sys


class Descriptor(ctypes.Structure):
    _fields_ = [
        ("size", ctypes.c_uint32),
        ("abi", ctypes.c_uint32),
        ("capability", ctypes.c_char_p),
    ]


for path, getter, expected in zip(
    sys.argv[1:],
    ("rptadv_portaudio_alsa_adapter_descriptor", "rptadv_gpio_adapter_descriptor"),
    ((208, 2, b"rptadv.portaudio-alsa-audio"), (360, 1, b"rptadv.cm119-hid-gpio")),
    strict=True,
):
    library = ctypes.CDLL(path)
    describe = getattr(library, getter)
    describe.argtypes = ()
    describe.restype = ctypes.POINTER(Descriptor)
    result = describe().contents
    actual = (result.size, result.abi, result.capability)
    if actual != expected:
        raise RuntimeError((path, actual, expected))
    print(path, *actual)
PY
sha256sum "$out/$audio" "$out/$gpio"
