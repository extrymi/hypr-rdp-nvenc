# CABAC fixtures for VALIDATION-PLAN B3-2

Independent-encoder H.264 streams for `src/nvenc/annexb.rs` (module `independent_cabac_stream`).

| File | SHA-256 |
| --- | --- |
| `cabac.264` | `976d1e346cb2584cf968c2b17a004506d1d85905c982e7a1544bda634244542e` |
| `cabac-zw1.264` | `a67ee1a2287eeb5e505ea595345c2bd19df8d03b65f27200bdb8b58215f9e693` |
| `cabac-zw2.264` | `e4bc1e9e9e23365cf36c344c57d25161610fd70cfd1d2057c0d77b8e0e0e1935` |

Generated on a disposable GitHub runner, never on a workstation: workflow `Fixture generation`
(branch `impl/r1-evidence`, commit 6e6ce3348aa9e26d2ebb5987b29efc29cd91fc4d, run 40834275533). Tools: Ubuntu 24.04 packages
ffmpeg 7:6.1.1-3ubuntu5 and libx264-164 2:0.164.3108+git31e19f9-1. The runner stands in for the plan's E1.

1. `cabac.264` comes from `ffmpeg -f lavfi -i testsrc2=size=64x64:rate=30 -frames:v 3 -pix_fmt yuv420p -c:v libx264
   -profile:v high -x264-params cabac=1:bframes=0:keyint=30 -bsf:v h264_mp4toannexb -f h264`. It is High profile,
   4:2:0, 8-bit, with NAL units SPS, PPS, SEI, IDR, P and P, and its PPS sets entropy_coding_mode_flag.
2. `cabac-zwK.264` (K = 1, 2) is the same stream rewritten by `ci/fixtures/add_cabac_zero_words.py` (evidence
   branch), which shares no code with the Rust parser. It checks that the PPS sets entropy_coding_mode_flag,
   writes every NAL unit with a 4-byte start code, and appends K cabac_zero_words after each slice's last
   (stop-bit) byte, encapsulated as `(00 00 03)*K` (H.264 7.3.2.10, 7.4.1). `ci/fixtures/make-cabac.sh` runs
   steps 1 to 3.
3. `ffmpeg -f h264 -i <stream> -f framemd5` decodes all three streams to the same three frames: the framemd5
   files are byte-identical, sha256
   `88fa28d165fcbc1acb8db77c400ac1635b825f266126c16942d1f4c61ee19544`.

Deviation from the plan's command: `-pix_fmt yuv420p` is added to pin 4:2:0 explicitly.
