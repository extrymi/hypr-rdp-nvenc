# Native NVENC adapter

This stage implements the library Driver from ADR0001. It does not yet expose
NVENC in the desktop server's CLI or connect capture/EGFX to it. Enable the
internal `nvenc` build feature to compile the adapter and its ABI tests.
No workstation build, GPU probe, driver invocation or installation is required.

## Boundaries

`native::serve` checks the private worker marker and hands a lazy Driver to the
existing worker protocol. Library loading, maximum API query, CUDA enumeration,
context/session creation and all other foreign calls happen only after Init.
This preserves the supervisor's persist-before-driver-work invariant. The
adapter is neither Send nor Sync; it owns a dedicated current CUDA context on
the worker's OS thread. The parent process never calls or loads the driver.

Production loads only `libcuda.so.1` and `libnvidia-encode.so.1`. It does not
bundle or link a CUDA runtime, CUDA Toolkit, nvcc, NVIDIA driver or FFmpeg.
Driver API 13.1 is required; old API versions are rejected before creating the
function table. Struct versions come from the C preprocessor, never guessed
or downgraded. The two vendored interface headers are unmodified, checksum-pinned
and retain their original MIT permission/copyright notices.

## Fixed initial path

* Resolve the configured GPU UUID by enumeration; reject absence, duplicates,
  erroneous counts and query errors. Never silently select ordinal0.
* Query H.264, High profile, NV12, P1 preset, min/max geometry, macroblock/rate,
  CBR and CABAC support. Real initialization must also succeed.
* Use ultra-low-latency synchronous H.264, progressive SDR 8-bit AVC420, CBR,
  no B-frames, lookahead, AQ or external lookahead, and one-frame VBV. This
  intentionally narrow first backend does not implement CQP or quality controls.
  Future CLI integration must reject unsupported options, not silently ignore them.
* Allocate four independent input/bitstream pairs. Copy packed NV12 rows through
  the input lock using the returned destination pitch. No registration/mapping
  or DMA-BUF ownership is introduced in this first CPU-copy path.
* Only explicit unaccepted Busy input is retried, at most twice by Engine.
  NEED_MORE_INPUT is accepted input and terminates this zero-delay generation.
  Unknown submission errors remain potentially pending for terminal cleanup.
* Check output size, pointer, frame index, timestamp and picture type before
  copying/forwarding bytes. Match every successful input/output lock with one
  unlock attempt, including validation failures. Never repeat an ambiguous unlock.
* Attempt EOS once when draining or cleaning potentially pending input. Release
  all slots in reverse order, then session, then context, before unloading both
  libraries. Each destroy handle is removed from ownership before calling it.
  A failed acquisition cleans its partial resources, including failed probe.

Every foreign call may still block. The existing supervisor supplies the real
deadline/kill/reap boundary. Best-effort cleanup errors do not prove that a driver
released an object; confirmed process exit is required before retry/fallback.
This cannot isolate a desktop from a kernel/GPU fault on a shared physical GPU.

## Bitstream checks

The bounded Annex B parser checks start codes, NAL bounds/types, forbidden bit,
SPS/PPS IDs and references, progressive 8-bit 4:2:0 SPS dimensions, slice type,
PPS references, increasing slice starts and driver/bitstream IDR agreement.
Initial/recovery IDR requires current inline SPS and PPS. P frames carry cached
parameters for the upstream Windows decoder workaround; AUD stays first.
Parameter cache commits only after the access unit passes these checks.

These are structural checks for our fixed path, not a complete H.264 decoder.
They do not prove hardware-generated streams render correctly on real clients.
CPU tests generate valid frames with the bundled OpenH264 encoder, pass those
bytes through the independent C API fixture and normalization, and decode each
result with OpenH264. This is a framing/metadata/normalization oracle; the C
fixture does not emulate NVIDIA compression or prove GPU image fidelity.

## Build, tests and implementation map

`build.rs` generates Rust structs, unions, constants, function pointers and
Clang-derived layout tests from the pinned headers. Independent `abi.c` checks
C/Rust sizes, offsets and bitfield interoperation. Original C fixture code lives
in `tests/native/fake_driver.c`; it is thread-local, implements the C function
table and validates settings, pitch/padding and ownership traces. A separate
shared-object fixture exercises real dlopen/dlsym, old API and missing function
rejection. Neither fixture is a GPU implementation. Release symbol inspection
rejects fixture code in the server executable.

`native/loader.rs` owns libraries and validates symbols/tables. `native/driver.rs`
owns foreign resources and lifecycle. `annexb.rs` owns bounded framing/cache
validation. `native/tests.rs` covers ABI, loader, UUID choice, capability errors,
every acquisition stage, partial buffer allocation, unlocks, delayed input,
Busy retries, stale metadata, output bounds and cleanup error continuation.

Run only in the disposable CI environment:

```sh
cargo test --frozen --features nvenc --lib
cargo test --frozen --no-default-features --features nvenc --lib
cargo clippy --frozen --all-targets --all-features -- -D warnings
```

The draft PR and its CI logs record the actual validation of each commit.
Hardware qualification, production worker dispatch, capture conversion,
strict RDP/EGFX failure propagation and real Windows/Linux/macOS remain open.

## Sources and licenses

The implementation was written for this fork; it does not copy a third-party
encoder implementation. It uses the pinned headers' API contracts and the
[NVIDIA 13.1 programming guide](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.1/nvenc-video-encoder-api-prog-guide/index.html).
The input acceptance/cleanup lessons and alternative discussion are in ADR0001.
`cc` 1.4.2 and `libloading` 0.8.9 are already locked packages, now direct optional
build/runtime dependencies. No package version/checksum changes are needed.
Their licenses and those of all bundled dependencies still need distribution
inventory in the packaging stage. Original project/OpenH264 LICENSE is retained.

No system deployment is performed. The change is a stacked draft branch; closing
it rolls back no host state. Main remains frozen. Package rollback/removal and
real GPU/context recovery are separate, currently unexecuted acceptance gates.
