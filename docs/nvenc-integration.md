# Native NVENC server integration (hardware unqualified)

The optional `nvenc` feature now connects the server's CPU capture/AVC420 path
to the same-executable private worker. This is a development build. CPU tests
do not establish NVIDIA driver, desktop, client or installation qualification.
The Arch package installs this document; NVIDIA hardware, real clients and
installation on a real workstation remain unqualified.

## Configuration

In a build compiled with `--features nvenc`, the narrow initial configuration is:

```toml
h264_backend = "nvenc"
nvenc_device = "GPU-01234567-89ab-cdef-0123-456789abcdef" # replace with actual UUID
egfx_codec = "avc420"
rate_control = "cbr"
bitrate = 10000000
fps = 30
```

Keep the existing authentication/TLS and output settings. Remove an explicit
`quality` setting: this backend implements bitrate-controlled CBR, not a quality
or CQP knob. CBR uses the existing bitrate-controlled EGFX metadata hint (QP zero),
not a claimed measurement of NVIDIA quantization. VBR/CQP, AVC444,
ClearCodec and ambiguous codec selection are rejected for this initial native
path. Omitting rate_control chooses CBR only when an NVENC device is selected;
the historical default elsewhere stays VBR. FPS is 1..240 and bitrate is nonzero.

`nvenc_device` requires the stable full GPU UUID; ordinals and DRM paths fail.
With `h264_backend="auto"` and an explicit UUID, the selected device is treated
as NVIDIA: NVENC is tried, never VAAPI/DMA-BUF. A classified backend failure can
fall back to software only after confirmed child exit. Software recovery uses
the existing VBR encoder and logs the mode change. Protocol, transport, state
store, timeout and quarantine errors end the connection. Auto without a UUID
retains historical VAAPI/software selection; the parent does not probe CUDA.
The software/VAAPI modes reject an unrelated nvenc_device setting.

`--check-config` validates build support and these options without loading a
driver, creating worker state, discovering a compositor or opening a listener.
Actual capabilities and encode remain runtime/hardware acceptance checks.

## Process and frame ownership

The private worker argument and marker are validated before server configuration
and before Tokio runtime creation. fd3 must be a connected Unix stream whose
peer is the same-UID parent. Standalone/malformed dispatch fails. Production has
no driver-library override, fake encoder option or child environment inheritance.
The parent persists its device lease record before Init can trigger native work.

The blocking Wayland capture thread owns a current-thread supervisor runtime.
It waits for exactly one IPC encode exchange with existing absolute deadlines;
all foreign driver work stays in the child. This facade rejects use from an
async executor instead of nesting a runtime. Normal destruction drains, destroys,
waits and reconciles the lease; cancellation retains the restart-safe record.

Packed capture formats are normalized to BGRA, then the existing BT.709 full
range planar conversion is reused and UV is interleaved as tightly packed NV12.
Row padding is respected. WLR YInvert is applied before scaling/damage processing,
including owned storage for a deferred inverted frame. Unknown flags/formats fail.
The [WLR protocol](https://github.com/swaywm/wlr-protocols/blob/master/unstable/wlr-screencopy-unstable-v1.xml)
defines this buffer orientation flag.

Each reconnect, negotiated generation or presentation geometry replacement drops
the previous encoder before starting a new lease/worker. The first frame and
recovery after a dropped encoded frame require IDR. Auto software recovery resets
damage/reference state and requests a full IDR. Strict native/VAAPI errors end
capture and use the existing capture-death connection termination; they do not
substitute bitmap output. A strict backend also refuses a client without AVC.
This tears down the remote connection, not Hyprland or the user's applications.

## Operation and diagnostics

A strict backend (`h264_backend = "nvenc"` or `"vaapi"`) never substitutes
bitmaps or software encoding. It ends capture and the RDP connection and logs
why: the client did not negotiate AVC; the graphics pipeline was not opened
within the activation grace (2 s) or was closed; the AVC encoder or surface is
unavailable; the encoder failed to initialize
(`Failed to initialize H.264 encoder: …`) or to encode
(`NVENC failed; ending capture session`, or
`Forced encoder failed; ending capture session` for VA-API); or it produced
output too short to send (`… produced no usable output; ending capture session`).
The server closes only that connection and keeps listening. It does not stop
Hyprland or the applications, but it cannot isolate them from a kernel or GPU
fault on the shared device (see below).

With `h264_backend = "auto"` and an `nvenc_device`, an `NVENC: Backend { … }`
failure, including refused output, switches to software VBR once the worker's
exit is confirmed
(`NVENC startup failed after confirmed exit; using software VBR`, or
`NVENC failed and its worker was reaped; recovering with software VBR and a full IDR`).
Native output too short to send and every other NVENC error (timeout, I/O,
protocol, state store, quarantine) end the connection in this mode too.

Output refused by the worker appears in these lines as
`NVENC: Backend { phase: Encode, fault: InvalidOutput, code: N }`. N names the
check: 1 input buffer mapping/pitch, 2 bitstream pointer, 3 bitstream size,
4 hwEncodeStatus, 5 output timestamp, 6 frameIdx, 7 picture type, 8 Annex B
structure, 9 emulation prevention, 10 SPS size (displayed size or coded bounds),
11 IDR agreement (including a requested IDR answered with a non-IDR picture),
12 engine bounds; 0 is reserved and not produced by the current code. NVENC
(NVENCSTATUS) and CUDA (CUresult) status codes appear only with `fault: Driver`
or `fault: DeviceUnavailable`. If the parent cannot reap a failed worker within
1 s, `NVENC: Quarantined` is logged instead, and every later NVENC connection
fails until that worker is gone (normally a reboot); do not delete its record
(see Rollback). The worker and the PAM helper execute `/proc/self/exe` with
argv[0] `hypr-rdp`: their process name (comm, journald `_COMM`) is `exe`, and
`pgrep -f hypr-rdp` finds them.

Follow the package's `arch-package.md`: stop `hypr-rdp-nvenc.service` before a
package change and start it again after preflight. If a running service is
upgraded anyway, its workers and PAM helpers still execute the old image
(`/proc/self/exe`) but load the current shared libraries and PAM configuration;
restart it (`systemctl --user restart hypr-rdp-nvenc.service`) after upgrading
this package or its libraries. Reboot after an NVIDIA driver upgrade: new
workers load the new userspace driver while the old kernel module is still
loaded, which is expected to fail NVENC initialization (not tested on
hardware). Strict mode then ends each new connection. Auto mode with an
`nvenc_device` falls back to software only if the driver reports an error and
the worker exits; a hang or crash ends the connection.

Frames are charged to the local EGFX window under the IronRDP pipeline lock and
only while IronRDP tracks them. A completed graphics reset (client CapsAdvertise,
resize, channel close) or an acknowledgement suspension empties both, so the
stream does not wait for acknowledgements that will never arrive.

Use `auth_mode = "configured"` (CredSSP/NLA) with a long random password for
remote access. The pinned IronRDP sets no TCP keepalive in any mode. In the
TLS-only modes (`pam`, `none`) it serves one connection at a time and leaves
others in the listen backlog; the X.224/TLS handshake has no deadline, so one
silent or half-open connection blocks every other client. In `configured` mode
a newcomer that authenticates within 10 s replaces the current connection,
including a silent one. In `pam` mode the packaged PAM service includes Arch's
`system-auth`, which also runs `pam_faillock` for sudo and login. Whether remote
failures from the helper (it runs as the desktop user) count toward that local
lockout is not tested.

## CPU evidence and open gates

CI tests the actual executable's private dispatch/Hello without sending Init,
the blocking supervisor with CPU child fixtures, configuration refusal before
desktop access, pitched color conversion, orientation, and a CPU OpenH264 worker
oracle with encode/decode and IDR recovery. An EGFX regression covers native
failure, strict disconnect and software recovery. These fixtures never invoke
the real NVIDIA API. Existing generated ABI and C API fixture tests remain.

The PR/CI record the actual result. Real NVIDIA compression, image fidelity,
Windows/Linux/macOS, reconnect/resize/soak, locking/suspend, Arch package lifecycle
and WOL/pre-login remain open. Process deadlines cannot isolate the desktop from
a kernel/GPU fault on a shared device. DMA-BUF/CUDA import is a separate stage.

## Rollback

For the package, follow the upgrade and rollback steps in `arch-package.md`.
Without changing packages, choose `h264_backend="software"` and remove
`nvenc_device`/CBR; the software path does not probe the GPU. Do not delete a
live worker record to force recovery. Package lifecycle is exercised only in a
disposable CI container; rollback on real hardware is not tested.
