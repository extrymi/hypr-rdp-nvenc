# CPU baseline for the NVIDIA fork

This stage prepares reproducible source inputs and a disposable CPU CI build.
It adds no NVENC code and does not install or launch the server on the workstation.

## What is fixed

- Upstream: `MuNeNiCK/hypr-rdp` at `730020b8795b6f6a57a69f441414a80d2a8505d5` (0.1.7).
- Rust: `1.99.1`, minimal profile with rustfmt/clippy, target `x86_64-unknown-linux-gnu`.
- All 484 Cargo packages: existing unchanged `Cargo.lock`, registry checksums and IronRDP revision `3db08ba86983ce35c9d36ad33f59ceb4b607dce8`.
- Checkout and artifact upload Actions: full reviewed commit SHAs in `ci/baseline.json`.
- Runner family: GitHub-hosted `ubuntu-24.04`; this workflow never requests a self-hosted or GPU runner.

The verifier rejects drift in Cargo.toml/Cargo.lock, the toolchain file, Git dependencies or Actions pins. Intentional dependency changes must update the manifest explicitly in their own reviewed change. The source application and its runtime defaults remain upstream baseline in this stage.

## Isolation and limits

The build runs on an ephemeral GitHub Actions runner, with read-only repository token permissions and no persisted checkout credential. No repository secrets, production credentials, user home, desktop, VPN, host network or GPU devices are supplied. No caching action is used for the first baseline.

The workflow installs compilers/headers and enables FUSE only inside that disposable runner. It runs upstream tests, not a live desktop server. Do not run the dependency-installation or FUSE steps on the working Omarchy PC.

The runner image and Ubuntu package repositories evolve. This is a pinned Rust/Cargo/source baseline with recorded system provenance, not a claim of bit-for-bit reproducible native binaries. Each run records its image version, compiler/linker details, installed package versions and Cargo metadata. A failed run remains failed even when its partial diagnostics are uploaded.

The developer's restricted shell may lack GitHub DNS, Rust and CMake. Preparing files there does not constitute a successful build. Use the run and artifact from this repository's CI as execution evidence.

## Checks and artifacts

The pull request to `main` runs formatting, Clippy, all upstream default/software-only/software-with-file-clipboard tests with 512 generated cases, the existing ignored FUSE byte-copy integration test, and a frozen release build. Dependency download happens once with `cargo fetch --locked`; all compilation/tests use `--frozen`.

A green run must include every required step. A skipped, cancelled, disabled or approval-pending run is not a passing baseline. If fork Actions are disabled, enable Actions in this repository and rerun this pull request's check; no merge is required to review the branch.

The artifact includes test/build logs, environment and dependency provenance, ELF linkage, a checksum file and a tar archive preserving the binary mode. The archive is an upstream CPU baseline, not the NVENC implementation, an Arch package or a deployment release. GPU/NVENC, actual PAM/systemd/UFW, existing desktop, Windows/macOS, WOL and install/upgrade/purge gates remain open.

For an already isolated environment with the dependencies installed:

```sh
python3 scripts/ci/verify-baseline.py
rustup toolchain install 1.99.1 --profile minimal --component clippy --component rustfmt
cargo fetch --locked
cargo test --frozen
cargo test --frozen --no-default-features
cargo test --frozen --no-default-features --features client-to-server
cargo build --frozen --release
```

No command here is an instruction to install dependencies or experiment on the working desktop.
