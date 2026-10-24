#!/usr/bin/env bash
# Only inside the disposable Arch builder container, never on the workstation.
set -euo pipefail
[[ -e /.dockerenv && $EUID == 0 && ${ARCH_SNAPSHOT:-} =~ ^[0-9]{4}/[0-9]{2}/[0-9]{2}$ ]]
printf 'Server = https://archive.archlinux.org/repos/%s/$repo/os/$arch\n' "$ARCH_SNAPSHOT" > /etc/pacman.d/mirrorlist
pacman -Syu --noconfirm --needed rustup git clang cmake pkgconf python libva libpulse libxkbcommon mesa pipewire wayland pam fuse3 systemd openssl util-linux
useradd --create-home --uid 1100 builder
cp -r /source/pkg/arch /home/builder/recipe
chown -R builder:builder /home/builder/recipe
runuser -u builder -- rustup toolchain install 1.99.1 --profile minimal
runuser -u builder -- bash -c 'cd /home/builder/recipe; makepkg --cleanbuild --check --noconfirm --log'
recipe=/home/builder/recipe
nm "$recipe/src/target/release/hypr-rdp" > /out/arch-symbols.txt
if grep -q hypr_mock_ /out/arch-symbols.txt; then
    echo 'CPU mock symbols in production executable' >&2; exit 1
fi
readelf -d "$recipe/src/target/release/hypr-rdp" > /out/arch-linkage.txt
if grep -E 'NEEDED.*lib(avcodec|avformat|avutil|swscale|swresample|openh264|cuda|nvidia-encode)' /out/arch-linkage.txt; then
    echo 'Unexpected external codec or driver linkage' >&2; exit 1
fi
cp "$recipe"/*.pkg.tar.zst /out/
runuser -u builder -- bash -ec 'cd /home/builder/recipe; makepkg --source --noconfirm; makepkg --printsrcinfo' > /out/arch-srcinfo.log
cp "$recipe"/*.src.tar.gz /out/
# Change package revision only; exercise real upgrade/downgrade without rebuilding
# unchanged Rust. The runtime and config payload remain the reviewed revision.
sed -i 's/^pkgrel=1$/pkgrel=2/' "$recipe/PKGBUILD"
runuser -u builder -- bash -c 'cd /home/builder/recipe; makepkg --repackage --force --noconfirm'
cp "$recipe"/*-2-x86_64.pkg.tar.zst /out/
pacman -Q > /out/arch-builder-packages.txt
runuser -u builder -- rustup run 1.99.1 rustc -vV > /out/arch-rustc.txt
cc -Wall -Wextra -Werror /source/scripts/ci/arch/pam-probe.c -lpam -o /out/pam-probe
for package in /out/*.pkg.tar.zst; do
    bsdtar -xOf "$package" .PKGINFO > "$package.PKGINFO"
    bsdtar -xOf "$package" .BUILDINFO > "$package.BUILDINFO"
    bsdtar -tf "$package" > "$package.files"
    if bsdtar -tf "$package" | grep -Eq '^\.?/?(\.INSTALL|usr/lib/systemd/system/|usr/lib/systemd/user/.*\.wants/|etc/[^p]|etc/p[^a]|etc/pam.d/[^h])'; then
        echo 'Unexpected package side-effect surface' >&2; exit 1
    fi
done
python3 - <<'PY'
import hashlib, json, os
from pathlib import Path
root = Path('/out')
(root / 'ARCH-BUILD-PROVENANCE.json').write_text(json.dumps({
    'run_id': os.environ['GITHUB_RUN_ID'], 'tested_merge': os.environ['GITHUB_SHA'],
    'runtime_source': 'fd4b702b7e7454d98eda0d155a75c4b0c56c2092',
}, indent=2) + '\n')
files = [p for p in sorted(root.iterdir()) if p.is_file() and p.name not in {'arch-build.log', 'ARCH-BUILD-SHA256SUMS'}]
(root / 'ARCH-BUILD-SHA256SUMS').write_text(''.join(f'{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n' for p in files))
PY
