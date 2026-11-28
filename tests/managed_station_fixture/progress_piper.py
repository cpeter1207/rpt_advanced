#!/usr/bin/python3
"""Private mount-namespace speech witness for the outgoing SIP terminal fixture."""
import math
import struct
import sys
from pathlib import Path


def main():
    assert len(sys.argv) == 6
    assert sys.argv[1] == "--model"
    assert sys.argv[3:5] == ["--output_raw", "--length_scale"]
    model = Path(sys.argv[2])
    assert model.name == "progress-80.onnx" and model.read_bytes() == b"synthetic voice\n"
    text = sys.stdin.buffer.read()
    category = "busy" if text == b"The line is busy." else "other"
    with model.with_suffix(".requests").open("a", encoding="ascii") as requests:
        requests.write(category + "\n")
    if category == "busy":
        wave = b"".join(struct.pack("<h", round(8192 * math.sin(2 * math.pi * i / 8)))
                        for i in range(8))
        sys.stdout.buffer.write(wave * 800)  # 800 ms at 8 kHz


if __name__ == "__main__":
    main()
