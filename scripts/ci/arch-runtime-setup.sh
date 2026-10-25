#!/usr/bin/env bash
set -euo pipefail
[[ -e /.dockerenv && $EUID == 0 && ${ARCH_SNAPSHOT:-} =~ ^[0-9]{4}/[0-9]{2}/[0-9]{2}$ ]]
printf 'Server = https://archive.archlinux.org/repos/%s/$repo/os/$arch\n' "$ARCH_SNAPSHOT" > /etc/pacman.d/mirrorlist
# Official container images omit documentation with NoExtract. A normal installed
# Arch system does not: exercise the package's real documentation/license payload.
sed -i '/^[[:space:]]*NoExtract[[:space:]]*=/d' /etc/pacman.conf
# Fresh root: no compiler or build dependencies can hide missing runtime linkage.
pacman -Syu --noconfirm --needed systemd dbus python iproute2
packages=(/out/hypr-rdp-nvenc-*-1-x86_64.pkg.tar.zst)
[[ ${#packages[@]} == 1 && -f ${packages[0]} ]]
pacman -U --noconfirm "${packages[0]}"
pacman -Q > /out/arch-runtime-packages.txt
ldd /usr/bin/hypr-rdp | tee /out/arch-runtime-linkage.txt
! grep -q 'not found' /out/arch-runtime-linkage.txt
[[ ! -e /dev/nvidia0 && ! -e /usr/lib/libnvidia-encode.so.1 ]]
useradd --create-home --uid 1100 tester
echo 'tester:disposable-ci-password' | chpasswd
install -Dm755 /out/pam-probe /usr/local/libexec/hypr-rdp-ci/pam-probe
install -Dm755 /source/scripts/ci/arch/service-oracle.py /usr/local/libexec/hypr-rdp-ci/service-oracle
cp /source/scripts/ci/arch/lifecycle.py /root/hypr-rdp-lifecycle.py
pacman -R --noconfirm hypr-rdp-nvenc
systemd-machine-id-setup
# This root is a systemd/PAM test appliance, not a hardware boot. Do not run
# device discovery, module loading or console gettys on the shared runner kernel.
systemctl mask systemd-udevd.service systemd-udev-trigger.service systemd-modules-load.service getty@tty1.service
