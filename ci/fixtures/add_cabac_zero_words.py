#!/usr/bin/env python3
"""Append k cabac_zero_words to every CABAC slice NAL of an Annex B stream.

H.264 (V16) 7.3.2.10 allows cabac_zero_word (0x0000) after the slice trailing
bits only when the PPS sets entropy_coding_mode_flag. 7.4.1 encapsulates the
RBSP: an emulation prevention byte follows every 0x0000 that precedes a byte
<= 3, and a final 0x03 follows an RBSP that ends in 0x0000, so k words become
(00 00 03) * k. This file shares no code with the Rust parser under test.
"""
import sys


def nals(data):
    starts, i = [], 0
    while i + 3 <= len(data):
        if data[i:i + 3] == b"\0\0\1":
            starts.append(i + 3)
            i += 3
        else:
            i += 1
    out = []
    for n, start in enumerate(starts):
        end = starts[n + 1] - 3 if n + 1 < len(starts) else len(data)
        nal = data[start:end].rstrip(b"\0")  # trailing_zero_8bits and 4-byte start codes
        out.append(nal)
    return out


def rbsp(nal):
    out, zeros = bytearray(), 0
    for byte in nal[1:]:
        if zeros >= 2 and byte == 3:
            zeros = 0
            continue
        out.append(byte)
        zeros = zeros + 1 if byte == 0 else 0
    return bytes(out)


def entropy_coding_mode_flag(pps):
    bits = "".join(f"{b:08b}" for b in rbsp(pps))
    pos = 0
    for _ in range(2):  # pic_parameter_set_id, seq_parameter_set_id: ue(v)
        zeros = bits.index("1", pos) - pos
        pos += 2 * zeros + 1
    return bits[pos] == "1"


def main(source, target, k):
    units = nals(open(source, "rb").read())
    kinds = [u[0] & 31 for u in units]
    assert 8 in kinds and all(entropy_coding_mode_flag(u) for u in units if u[0] & 31 == 8), "PPS must be CABAC"
    out = bytearray()
    for unit in units:
        if unit[0] & 31 in (1, 5):
            assert unit[-1] != 0, "slice must end in its rbsp_stop_one_bit byte"
            unit = unit + b"\0\0\3" * k
        out += b"\0\0\0\1" + unit
    open(target, "wb").write(out)
    print(f"{target}: {kinds.count(1) + kinds.count(5)} slices, k={k}")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2], int(sys.argv[3]))
