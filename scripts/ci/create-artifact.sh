#!/usr/bin/env bash
set -euo pipefail

# Run only after the frozen release build on the disposable CPU runner.
mkdir -p artifacts/build-output
cp target/release/hypr-rdp artifacts/build-output/
cp LICENSE README.md Cargo.toml Cargo.lock rust-toolchain.toml artifacts/build-output/
cp ci/baseline.json artifacts/build-output/
cat > artifacts/build-output/NOT-FOR-DEPLOYMENT.txt <<'NOTICE'
CPU baseline build of upstream hypr-rdp 0.1.7.
This artifact contains no NVENC implementation or deployment configuration.
It is not an Arch package and has not passed GPU, live-desktop, PAM, network,
Windows/macOS or installation/removal qualification. Do not treat it as the
finished fork or deploy it on the workstation as part of this stage.
NOTICE
target/release/hypr-rdp --version > artifacts/binary-version.txt
readelf -d target/release/hypr-rdp > artifacts/linkage.txt
if grep -E 'NEEDED.*lib(avcodec|avformat|avutil|swscale|swresample|openh264)' artifacts/linkage.txt; then
  echo "Unexpected external codec dependency" >&2
  exit 1
fi
(
  cd artifacts/build-output
  sha256sum hypr-rdp LICENSE README.md Cargo.toml Cargo.lock rust-toolchain.toml baseline.json NOT-FOR-DEPLOYMENT.txt > SHA256SUMS
)
tar -czf artifacts/cpu-baseline-linux-x86_64.tar.gz -C artifacts/build-output .
rm -r artifacts/build-output
(
  cd artifacts
  sha256sum cpu-baseline-linux-x86_64.tar.gz > cpu-baseline-linux-x86_64.tar.gz.sha256
)
