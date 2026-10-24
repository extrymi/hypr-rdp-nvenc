# NVENC development map

Start with [ADR0001](adr/0001-nvenc-worker.md) for decisions and boundaries.
The library is an internal implementation boundary, not a stable external API.

| File | Responsibility |
| --- | --- |
| `src/nvenc/types.rs` | Validated device/configuration, frame identity, typed faults |
| `src/nvenc/policy.rs` | Pure backend selection/fallback rules |
| `src/nvenc/engine.rs` | Driver ownership, accepted-input semantics, resource lifecycle |
| `src/nvenc/protocol.rs` | Bounded versioned packets, strict decoding |
| `src/nvenc/lease.rs` | Private device lock and restart-safe process reconciliation |
| `src/nvenc/supervisor.rs` | Parent deadlines, process termination, worker loop |
| `src/nvenc/tests.rs` | Unit/property tests with a fake driver |
| `tests/nvenc_worker.rs` | Actual subprocess tests; `fixture_*` entries run only as children |
| `tests/support/mod.rs` | Shared fake driver, no native GPU access |

In the disposable CPU environment prepared by CI:

```sh
cargo test --frozen --lib
cargo test --frozen --test nvenc_worker
cargo clippy --frozen --all-targets --all-features -- -D warnings
```

The full workflow also runs the original binary tests under default, software
and software+clipboard features and performs a release build. No Rust/CUDA
installation or test run on the working desktop is required by this stage.

Next implement the native Driver with generated ABI checks and mocked foreign
calls. Then connect the same-executable hidden worker entry point, configuration
and EGFX policy. Do not expose `nvenc` as a working server option before those
paths and strict failure propagation exist. Existing authorization/TLS remain
in the server; configuration error handling is a separate, narrowly scoped patch.
