# ADR 0001: native NVENC behind a supervised worker

Status: accepted for implementation, 2026-10-24. Hardware qualification remains open.

## Decision and alternatives

Implement one native CUDA-driver/NVENC adapter, loaded dynamically inside an
unprivileged worker of the same executable. The first path accepts CPU NV12 and
produces H.264 AVC420 SDR 8-bit. There are no custom CUDA kernels, CUDA Toolkit
runtime dependency, DMA-BUF imports, AVC444, or dynamic reconfiguration in this
first path. Resize creates a new immutable encoder generation.

The alternative is system libavcodec with `h264_nvenc`. It offers a mature
adapter and status handling, at the cost of its API/ABI/dependency closure and a
more general send/receive pipeline. Both approaches add zero runtime packages
on the inventoried workstation, where FFmpeg and the NVIDIA driver already
exist. Native therefore has no claimed installation saving on that machine.
On a clean Arch installation it avoids introducing FFmpeg solely for encoding,
but requires maintaining a smaller, carefully reviewed unsafe API boundary.
This is a scope/dependency decision, not evidence that a new native adapter is
more reliable than FFmpeg. Revisit this ADR if ABI or driver qualification fails.

Use generated bindings from the released `nv-codec-headers` tag `n13.1.15.0`,
peeled commit `0a6fba9a2820628b8103464f4c8753ee05838baa`, with original notices.
Before native integration, add C/Rust layout checks and a runtime maximum API
version/capability check. Never force a lower version field into a newer struct.
No headers or native bindings are included in this lifecycle-only change.

## Why a process

Linux NVENC output retrieval is synchronous. A Rust future/thread timeout cannot
cancel a blocking foreign call. The parent must not call the driver, including
probe, initialization, buffer allocation, output retrieval, EOS, or destruction.
The worker owns those operations on one OS thread, including its current CUDA
context. It has the same UID; it is not a privileged service.

A private Unix socket pair at inherited fd3 carries versioned messages. The
parent clears the child's environment and standard streams and adds one fixed
internal worker marker. The worker receives
device UUID, epoch, dimensions, rate settings and NV12 bytes; it receives no
authentication password, TLS key, compositor pointers or GPU handles.

One absolute timeout covers each complete exchange, including a blocked write
or partial reply. On failure the parent invalidates the generation, kills and
waits for the child within a separate reap budget. Drop requests a kill through
Tokio's child handle, while the on-disk record continues to protect the device.
Explicit `shutdown` is required for observable cleanup results.

These are user-space wait limits. They cannot recover a GPU/kernel D-state hang
or isolate Hyprland from faults in a shared GPU. Tests use fake drivers only.

## Flow and ownership invariants

- A configuration has one stable NVIDIA UUID, nonzero epoch and fixed geometry.
  No ordinal0 or DRM-node guess is accepted. UUID resolution and real capability
  checks belong in the native adapter.
- Input is tightly packed, even-sized NV12, at most64MiB, dimensions at most8192
  and frame rate at most240. These are application bounds, not hardware claims.
  Conversion of capture format, orientation, stride and colour remains a later
  integration step. The driver handles its own destination pitch.
- Four slots are allocated, independently of a single accepted frame in flight.
  Successful allocations are released once in reverse order. A failed native
  allocation cleans its own partial objects; destroy tolerates partial init.
- A generation accepts consecutive sequence numbers starting at0. Every reply
  echoes epoch and sequence. Stale replies are fatal and never become video.
- Cancelling an encode/shutdown future makes the generation terminal before
  the first await. Later calls cannot resubmit ambiguous input or repeat drain;
  explicit shutdown still performs bounded termination and reap.
- Only `Busy` means input was not accepted; at most two retries are allowed.
  `Delayed` means accepted input and terminates this zero-delay MVP with a typed
  error. It causes neither resubmission nor an output-lock wait.
- Output is capped at16MiB, and the first frame/explicit IDR request must report
  IDR. The native adapter must independently validate and normalize Annex B,
  current SPS/PPS and the upstream Windows parameter-set workaround. Test byte
  fixtures do not prove valid H.264 or actual client rendering.
- Drain stops submissions, destroy releases resources, then Exit waits for
  process termination. Failures are terminal for the generation. Release errors
  do not prevent attempts to release the remaining resources; ambiguous native
  operations are not repeated from Drop.

## Quarantine survives a parent restart

Acquire an exclusive nonblocking `flock` per UUID in a dedicated UID-owned0700
runtime directory. Before sending Init, persist a0600 record with boot ID, child
PID and `/proc/PID/stat` start time. Until Init the worker does no driver work.
If the parent dies before persistence, socket EOF terminates the uninitialized
worker. If it dies later, releasing the lock does not erase the worker record.

A subsequent parent refuses a new worker while the recorded process identity
still exists. An absent process, different start time (PID reuse), or new boot
permits reconciliation. Read/parse/permission errors fail closed. No process is
ever killed using a PID read from a record. The supervisor only signals its own
Child handle. A failed kill/reap retains the marker and blocks hardware attempts;
restarting the service is not a quarantine reset. The runtime directory is
expected to be recreated by the OS at boot; malformed records need diagnosis.

## Selection and failure policy

| Request / selected device | Attempt plan |
| --- | --- |
| software | Software only; no GPU probing |
| nvenc | One NVENC worker; no other backend |
| vaapi | Existing explicit VAAPI path; no NVENC probing |
| auto / NVIDIA | NVENC, then software after a classified failure and confirmed reap |
| auto / VAAPI device | Existing VAAPI, then software |
| auto / unknown | Software |
| device quarantined | Auto/software may use software; explicit hardware fails |

Quarantine also blocks VAAPI as a route to the same selected device. A strict
NVENC session will require AVC420 client capability, with no software,
ClearCodec or bitmap substitution. Transport/protocol/state-store errors are
not ordinary auto-fallback signals. This pure policy is tested now; existing
server selection and RDP fatal-error propagation are wired in a later change.

## Evidence and limits

`src/nvenc` contains a real engine contract, private IPC, process supervisor and
device lease, plus unit and subprocess tests. It adds an automatically detected
library target, with no Cargo dependency/lockfile changes. The current RDP binary
has no entry point into this library and loads no NVIDIA libraries.

Subprocess fixtures exercise hangs at startup, probe, init, allocation, submit,
output, drain, buffer release, destroy and process exit. They also exercise non-reading peers,
partial/oversized replies, stale epochs, terminal failure after N frames, and
repeated cleanup. Ignored `fixture_*` test entries are invoked explicitly by
their parent integration tests. Without the worker marker they return immediately;
a separate CI step verifies that a direct ignored-fixture run cannot hang.

Native loading, ABI, real device enumeration, valid bitstream/decode, desktop,
Windows/Linux/macOS clients and GPU fault recovery remain separate gates.

## Primary references and lessons

- [NVIDIA API guide: operation modes and cleanup](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-video-encoder-api-prog-guide/index.html): synchronous Linux retrieval and explicit buffer/session ownership.
- [Released header pin](https://github.com/FFmpeg/nv-codec-headers/tree/0a6fba9a2820628b8103464f4c8753ee05838baa): use generated ABI and distinguish accepted delayed input from busy/unaccepted input.
- [FFmpeg NVENC implementation](https://github.com/FFmpeg/FFmpeg/blob/bf0e831606938f530326c9679591109407fa8d0d/libavcodec/nvenc.c): model status/queue semantics explicitly; do not copy its delayed pipeline into a synchronous one-frame API.
- [Sunshine ownership](https://github.com/LizardByte/Sunshine/blob/d0b4a88ef727f8dabf80cc970a14739b55c885de/src/video.cpp): keep encoder and owning-device lifetimes ordered.
- [Upstream H.264 normalization](https://github.com/MuNeNiCK/hypr-rdp/blob/730020b8795b6f6a57a69f441414a80d2a8505d5/src/egfx/h264.rs): preserve its Windows SPS/PPS handling during integration.

No code was copied from these projects in this change.
