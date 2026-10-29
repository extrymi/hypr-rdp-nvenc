# Arch package: experimental NVIDIA fork

This package includes native NVENC, VA-API and inbound clipboard support. It is
not yet qualified on real NVIDIA hardware or Windows/Linux/macOS clients. CPU
package tests cannot establish desktop safety, image fidelity or performance.
The old `pkg/aur*` recipes still describe upstream; this recipe is the fork.

## Build and inspect

On a separate disposable Arch builder, obtain this reviewed packaging branch,
then run from `pkg/arch` with base-devel and the declared build dependencies:

```sh
makepkg --syncdeps --cleanbuild --check
makepkg --printsrcinfo
pacman -Qip ./hypr-rdp-nvenc-*.pkg.tar.zst
pacman -Qlp ./hypr-rdp-nvenc-*.pkg.tar.zst
```

Rust 1.99.1 is the tested toolchain. With rustup, install that toolchain on the
builder first. A distro Rust installation must provide the same compatible
compiler; the recipe does not download/install a toolchain or bypass pacman.
`prepare()` fetches locked Cargo dependencies; build/check use `--frozen`.
Runtime source is pinned to the CPU-tested runtime commit
`a7420471d711aea20dc5e311361142d2490da414`. Local unit/template/helper/doc sources
have SHA-256 checksums. Git is pinned to a full commit (its checksum is SKIP, as
usual for makepkg VCS sources). No floating runtime branch is built. A later
runtime change requires updating that explicit pin and retesting the package.

CI builds inside a pinned official Arch container against a dated signed Arch
repository snapshot; `ci/arch.json` declares both. Artifacts contain the package,
source recipe bundle, `.PKGINFO`, `.BUILDINFO`, file inventory, hashes, linkage,
compiler/package provenance and lifecycle logs. Packages are not signed releases;
compare the artifact's recorded hash before installing a downloaded package.
Build provenance is recorded, not a claim of bit-for-bit reproducible binaries.
CI may reuse a checked package build only for the same recipe/source files,
toolchain/image/snapshot pins, builder script, PAM probe and workflow file.
Output hashes are checked after restore. ARCH-BUILD-PROVENANCE.json identifies
the originating run and the runtime commit it built; a reused build also gets
ARCH-BUILD-REUSE.json, which states that build and tests were not re-executed.
Lifecycle tests always use a newly prepared runtime root. No Cargo target cache
or workstation state is reused.

## Install, without automatic activation

Deployment on the workstation remains a separate acceptance step. When a test
machine is available, select one exact inspected package file:

```sh
sudo pacman -U ./hypr-rdp-nvenc-0.1.7.nvenc2-1-x86_64.pkg.tar.zst
```

It conflicts with upstream hypr-rdp/hypr-rdp-git, shares the executable name and
PAM service, and provides hypr-rdp. Review pacman's replacement prompt and back
up an existing configuration and PAM customization before switching. A stray
`/usr/local/bin/hypr-rdp` can shadow the package in a terminal: inspect `type -a
hypr-rdp`; the unit always uses `/usr/bin/hypr-rdp`.

The package owns `/usr/bin/hypr-rdp`, `/usr/bin/hypr-rdp-tls`, one user unit,
`/etc/pam.d/hypr-rdp`, and its documentation/licenses. It has no install script,
presets, system service, socket activation, firewall rules or Omarchy config.
No credentials, certificate, user, groups, linger or enabled service are created.
NVIDIA is an optional runtime dependency: install a supported kernel driver and
matching nvidia-utils through the test system's normal package management.
API 13.1 needs driver 610+; a version number alone does not prove encoder caps.
CUDA toolkit and FFmpeg are not required by this backend.

Original MIT and NVIDIA interface-header notices are retained. Dependency notice
files are copied from the exact Cargo-resolved sources of the
x86_64-unknown-linux-gnu graph (`cargo metadata --filter-platform`). A crate's
own licence text is a LICENSE/LICENCE/COPYING/COPYRIGHT/NOTICE file at its
source root or its declared licence file. Deeper notices, such as the native
library notices of OpenH264, AWS-LC and ring, are kept but belong to vendored
code. A crate without its own text gets a verified upstream file from
`supplemental-licenses.json` (repository and commit recorded, SHA-256 checked at
build) or an explicit unresolved record; any other gap fails the build. The
dependency-licenses/index.json lists each crate's notices, its own notices and
the unresolved records with their evidence. It covers the whole graph,
including build and dev dependencies, and is not a legal compliance opinion.

Unresolved, with no text shipped or reconstructed: openh264 0.9.3 and
openh264-sys2 0.9.6 (the upstream README states only "Wrapper code is BSD-2,
Ralf Biedert" and publishes no licence text) and libva-sys 0.1.2 (Cargo.toml
says MIT; no licence text exists upstream). Do not distribute this package
until they are resolved.

## Configure the existing desktop

Run these steps as the user who owns the Hyprland session, not as root. Preserve
an existing configuration instead of copying over it:

```sh
install -d -m700 "$HOME/.config/hypr-rdp"
install -m600 /usr/share/doc/hypr-rdp-nvenc/config.example.toml "$HOME/.config/hypr-rdp/config.toml"
```

Replace all placeholders. Use the actual existing output name from `hyprctl
monitors` and the actual full NVIDIA UUID. An existing output keeps the desktop's
open applications visible; omitting `output` creates a separate headless output.
The example preserves the compositor keyboard layout and local audio playback.
There are no monitor power, input-disabling or lock/unlock hooks.

Set a dedicated RDP username and a nonempty password file owned by this user,
mode 0600, using a private editor. Use its absolute path; TOML does not expand
`~` or `$HOME`. Avoid putting passwords in command-line arguments or shell history.
Alternatively select `auth_mode="pam"` and remove username/password_file; the
packaged PAM service authenticates only access, not a new login session. The
server limits PAM admission to its own desktop user. Configured credentials use
CredSSP/NLA; PAM uses TLS and requires the client to submit credentials in that
mode. Real mstsc/PAM interoperability remains a separate acceptance gate.

`bind` initially selects loopback. Change it to the intended listening address
for remote access. Network access policy is managed separately by the operator;
this package neither installs subnet restrictions nor changes SSH/VPN/firewalls.
Strict NVENC requires AVC420, CBR, a full device UUID and no quality setting.

## TLS identity and client trust

Prefer an existing server certificate issued by a CA trusted by your clients,
with SANs matching the exact DNS name/IP used to connect. Supply both absolute
cert/key paths, with a readable chain and private key mode 0600. For a new test
self-signed identity, the optional packaged helper is an explicit manual action:

```sh
hypr-rdp-tls --dns rdp-test.example.net --ip 192.0.2.10
hypr-rdp --check-config --config "$HOME/.config/hypr-rdp/config.toml"
```

Replace these documentation addresses. The helper uses the default application
directory, also accepts `--directory ABSOLUTE_PATH`, and prints the SHA-256
certificate fingerprint, validity and SANs. The directory must be owned by the
current user with mode 0700. It refuses saved/partial identities and busy locks.
It never changes a client's trust store. Publication cannot replace an existing
entry; interruption between key/cert publication leaves an incomplete pair and
startup refuses it. Preserve or explicitly restore the matching pair.

Transfer only the public certificate to clients through a trusted channel.
Compare its SHA-256 fingerprint with the local helper output before trusting it.
For Windows, use the certificate manager for the connecting Windows user's
trusted certificates; for macOS use Keychain Access; for Linux use the RDP
client's certificate trust/pinning facility (its trust store may differ from the
OS store). Trust only this identity or its issuing CA and use its SAN name.
Do not use ignore-certificate options. Record any trust entry and any Windows
Credential Manager entry you add so those exact entries can later be removed.
Real client trust behavior, including self-signed-leaf acceptance, must still be
checked with the chosen client; provisioning success is not client acceptance.

The service's offline preflight requires an existing valid pair and cannot
generate one. Direct manual server startup retains upstream auto-provisioning
for localhost only; that identity is unsuitable for a different connection name.

## Start only in the current Hyprland session

After configuring and provisioning, run from that session's terminal:

```sh
systemctl --user import-environment WAYLAND_DISPLAY HYPRLAND_INSTANCE_SIGNATURE
systemctl --user start hypr-rdp-nvenc.service
systemctl --user status hypr-rdp-nvenc.service
journalctl --user -u hypr-rdp-nvenc.service -b
```

No global environment import is needed. The user manager already supplies the
session's XDG_RUNTIME_DIR and user bus. Refresh the two named values when changing
Hyprland sessions; the unit cannot choose between multiple desktops for you.
With a manager that tracks graphical-session.target, the unit stops with that
target. For sessions not managed that way, stop it before logout. No autostart,
linger, greeter changes or pre-login desktop is configured here.

Missing desktop environment skips startup. Failed config/auth/TLS ExecCondition
also skips it without retries (read the journal; inactive is not readiness).
Other failures retry at most three starts per 60 seconds, five seconds apart.
After fixing a runtime failure: `systemctl --user reset-failed
hypr-rdp-nvenc.service`, then start again. Type=exec confirms execution, not RDP
readiness or NVENC support. A connection is the hardware/client acceptance check.
SSLKEYLOGFILE is removed, UMask is 0077, and no sandbox flags disable PAM helpers
or driver device access. KillMode=mixed gives the main process 30 seconds to
shut down hooks/workers, then kills remaining processes in this unit's cgroup.

## Upgrade and rollback

Before each package change, stop the user service, retain the previous exact
package and a private copy of config/password/TLS/PAM changes, then use pacman -U
with the new exact file. Pacman preserves user config and marks PAM as a backup
file; inspect any `.pacnew`/`.pacsave` rather than replacing customizations blindly.
Run offline preflight before starting again. Package upgrade does not restart it.

For rollback, stop the service and install the retained previous package with
`sudo pacman -U /absolute/path/previous-package.pkg.tar.zst`. Restore its matching
config if needed, retain the TLS identity and repeat preflight. For GPU diagnosis
without changing packages, select h264_backend="software", remove nvenc_device,
and select rate_control="vbr". Strict native failure otherwise closes the remote
connection. Neither backend change nor reconnect is intended to close desktop apps.

CI tests revision-1 to revision-2 packaging upgrade and downgrade with the same
runtime, including modified PAM and stable TLS/user config. This is a package
transaction test, not proof that arbitrary future runtime versions are compatible.

## Remove and optional full cleanup

As the desktop user, first stop the service. If you previously enabled it yourself,
disable it. Then remove just the package:

```sh
systemctl --user disable --now hypr-rdp-nvenc.service
sudo pacman -R hypr-rdp-nvenc
systemctl --user daemon-reload
```

This intentionally leaves personal config, password, TLS identity and modified
PAM `.pacsave`. Shared dependencies, NVIDIA drivers and FFmpeg remain installed.
Avoid recursive dependency removal unless you have separately reviewed its list.

For full cleanup, review the installation's own file inventory and backups.
Remove only the config.toml/password/cert.pem/key.pem/.tls.lock you created in
`~/.config/hypr-rdp`, the exact unit enablement link/drop-ins you added, and the
modified `/etc/pam.d/hypr-rdp.pacsave` only if it belongs to this installation.
These names are shared with upstream: preserve them when returning to upstream.
Use `rmdir` for now-empty directories; do not recursively delete a user's config.
After interrupted provisioning, inspect any `.tls-stage.*` (helper) or
`.tls-pending-*` (server) directory. Once no provisioner is running, remove only
the files from that abandoned attempt; these directories can contain private keys.
Remove only this test identity's client trust entry and saved RDP credentials.

NVENC lease records live under `$XDG_RUNTIME_DIR/hypr-rdp-nvenc`. After a clean
stop, check for remaining server/worker processes and unresolved records. Never
delete a live/quarantined record to force GPU recovery. For a complete cleanup,
resolve the worker first (a test-machine reboot ends the runtime directory's
lifetime), then remove only that installation's leftover files. There is no
permanent root service, network rule, driver installation or Omarchy modification
from this package to undo. Keep backups only as long as intentionally required.

## Evidence boundaries

CI checks actual pacman install/upgrade/downgrade/remove/pacsave/full fixture
cleanup, declared runtime linkage in a fresh Arch root, real systemd user-unit
conditions/retry/stop behavior and real pam_unix authentication. A CPU oracle
stands in for ExecStart after the actual packaged binary's offline preflight.
The systemd test container shares the disposable runner kernel, not a real boot
or desktop. Hardware, RDP/PAM client flow, Omarchy session management, WOL and
pre-login without unsafe autologin remain unqualified. No test runs on the owner
workstation; main and the earlier draft branches remain untouched.
