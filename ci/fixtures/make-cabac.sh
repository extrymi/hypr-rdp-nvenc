#!/usr/bin/env bash
# VALIDATION-PLAN B3-2 on a disposable runner: a decodable CABAC stream from an
# independent encoder (libx264), the same stream with (00 00 03)*k tails, and
# an independent decoder (FFmpeg) that must produce identical frames.
set -euo pipefail
out=$1
mkdir -p "$out"
ffmpeg -hide_banner -version > "$out/ffmpeg-version.txt"
dpkg-query -W ffmpeg 'libavcodec*' 'libx264*' > "$out/packages.txt"
ffmpeg -hide_banner -nostdin -f lavfi -i testsrc2=size=64x64:rate=30 -frames:v 3 -pix_fmt yuv420p \
  -c:v libx264 -profile:v high -x264-params cabac=1:bframes=0:keyint=30 \
  -bsf:v h264_mp4toannexb -f h264 "$out/cabac.264" 2> "$out/encode.log"
for k in 1 2; do
  python3 ci/fixtures/add_cabac_zero_words.py "$out/cabac.264" "$out/cabac-zw$k.264" "$k"
done
for stream in cabac cabac-zw1 cabac-zw2; do
  ffmpeg -hide_banner -nostdin -f h264 -i "$out/$stream.264" -f framemd5 "$out/$stream.framemd5" 2> "$out/$stream.decode.log"
  grep -v '^#' "$out/$stream.framemd5" > "$out/$stream.frames"
done
[[ $(wc -l < "$out/cabac.frames") == 3 ]]
cmp "$out/cabac.frames" "$out/cabac-zw1.frames"
cmp "$out/cabac.frames" "$out/cabac-zw2.frames"
grep -q 'Error\|error while decoding' "$out"/*.decode.log && { echo "decoder reported errors" >&2; exit 1; }
(cd "$out" && sha256sum cabac.264 cabac-zw1.264 cabac-zw2.264 *.framemd5 > SHA256SUMS)
echo "B3-2: FFmpeg decodes all three streams to identical frames"
