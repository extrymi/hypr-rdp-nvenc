#!/usr/bin/env bash
# Only in the disposable Arch builder, after the current package is qualified.
set -euo pipefail
[[ -e /.dockerenv && $EUID == 0 && ${ARCH_SNAPSHOT:-} =~ ^[0-9]{4}/[0-9]{2}/[0-9]{2}$ ]]
printf 'Server = https://archive.archlinux.org/repos/%s/$repo/os/$arch\n' "$ARCH_SNAPSHOT" > /etc/pacman.d/mirrorlist
pacman -Syu --noconfirm --needed rustup git clang cmake pkgconf python libva libpulse libxkbcommon mesa pipewire wayland pam fuse3 systemd openssl util-linux
useradd --create-home --uid 1100 builder
old_recipe=d4ad349794bb4595c0f312f3aa10e757f2a6215f
install -d /home/builder/old-recipe /out/old-upgrade
git -c safe.directory=/source -C /source archive "$old_recipe" pkg/arch | tar -x -C /home/builder/old-recipe --strip-components=2
chown -R builder:builder /home/builder/old-recipe
runuser -u builder -- rustup toolchain install 1.99.1 --profile minimal
runuser -u builder -- bash -ec 'cd /home/builder/old-recipe
makepkg --cleanbuild --check --noconfirm --log'
runtime_source=$(runuser -u builder -- git -C /home/builder/old-recipe/src/hypr-rdp rev-parse HEAD)
[[ $runtime_source == fd4b702b7e7454d98eda0d155a75c4b0c56c2092 ]]
cp /home/builder/old-recipe/hypr-rdp-nvenc-0.1.7.nvenc1-1-x86_64.pkg.tar.zst /out/old-upgrade/
export OLD_RUNTIME_SOURCE=$runtime_source OLD_RECIPE_SOURCE=$old_recipe
python3 - <<'PY'
import hashlib, json, os, subprocess
from pathlib import Path
root = Path('/out/old-upgrade')
package = root / 'hypr-rdp-nvenc-0.1.7.nvenc1-1-x86_64.pkg.tar.zst'
binary = subprocess.run(['bsdtar', '-xOf', str(package), 'usr/bin/hypr-rdp'],
                        check=True, capture_output=True).stdout
assert binary.startswith(b'\x7fELF')
(root / 'OLD-BUILD-PROVENANCE.json').write_text(json.dumps({
    'run_id': os.environ['GITHUB_RUN_ID'], 'tested_merge': os.environ['GITHUB_SHA'],
    'recipe_source': os.environ['OLD_RECIPE_SOURCE'],
    'runtime_source': os.environ['OLD_RUNTIME_SOURCE'],
    'package': package.name, 'package_sha256': hashlib.sha256(package.read_bytes()).hexdigest(),
    'binary_sha256': hashlib.sha256(binary).hexdigest(),
    'note': 'Fresh rebuild of the exact old recipe; not the historical binary and not a reproducibility claim.',
}, indent=2) + '\n')
PY
