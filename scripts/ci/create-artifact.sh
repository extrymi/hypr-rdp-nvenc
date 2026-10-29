#!/usr/bin/env bash
set -euo pipefail

# Run only after the frozen release build on the disposable CPU runner.
# The public CPU artifact carries evidence about the executable, not the
# executable: it would need the dependency notices that only the Arch package
# collects. The recorded hash identifies the binary that CI built and tested.
mkdir -p artifacts/build-output
cp LICENSE README.md Cargo.toml Cargo.lock rust-toolchain.toml artifacts/build-output/
cp ci/baseline.json artifacts/build-output/
cp docs/native-nvenc.md docs/nvenc-integration.md docs/startup-validation.md artifacts/build-output/
mkdir -p artifacts/build-output/vendor
cp -r vendor/nv-codec-headers artifacts/build-output/vendor/
cat > artifacts/build-output/NOT-FOR-DEPLOYMENT.txt <<'NOTICE'
CPU development build evidence of the hypr-rdp fork, based on upstream 0.1.7.
The optional native NVENC worker/capture/AVC420 integration is compiled in.
Its GPU/driver/client behavior remains unqualified; see nvenc-integration.md.
The executable is not included: its dependency notices are not bundled here.
hypr-rdp.sha256 records the hash of the binary that CI built and tested.
This is not an Arch package and has not passed GPU, live-desktop, PAM, network,
Windows/macOS or installation/removal qualification.
NOTICE
(cd target/release && sha256sum hypr-rdp) > artifacts/hypr-rdp.sha256
target/release/hypr-rdp --version > artifacts/binary-version.txt
readelf -d target/release/hypr-rdp > artifacts/linkage.txt
if grep -E 'NEEDED.*lib(avcodec|avformat|avutil|swscale|swresample|openh264|cuda|nvidia-encode)' artifacts/linkage.txt; then
  echo "Unexpected linked codec or NVIDIA driver dependency" >&2
  exit 1
fi
(
  cd artifacts/build-output
  sha256sum LICENSE README.md Cargo.toml Cargo.lock rust-toolchain.toml baseline.json NOT-FOR-DEPLOYMENT.txt native-nvenc.md nvenc-integration.md startup-validation.md vendor/nv-codec-headers/* > SHA256SUMS
)
python3 - <<'PY'
from pathlib import Path

executables = [str(p) for p in Path("artifacts").rglob("*")
               if p.is_file() and p.open("rb").read(4) == b"\x7fELF"]
if executables:
    raise SystemExit(f"no executable may be published without its dependency notices: {executables}")
PY
tar -czf artifacts/cpu-baseline-linux-x86_64.tar.gz -C artifacts/build-output .
rm -r artifacts/build-output
(
  cd artifacts
  sha256sum cpu-baseline-linux-x86_64.tar.gz > cpu-baseline-linux-x86_64.tar.gz.sha256
)
