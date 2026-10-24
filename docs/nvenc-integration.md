# Native NVENC server integration (hardware unqualified)

The optional `nvenc` feature now connects the server's CPU capture/AVC420 path
to the same-executable private worker. This is a development build. CPU tests
do not establish NVIDIA driver, desktop, client or installation qualification.
No workstation deployment or package installation is part of this stage.

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
store, timeout and quarantine errors terminate the session. Auto without a UUID
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

This is a stacked draft branch with main frozen; no host state was installed.
Returning to the prior tested branch removes this source change. Future users
can choose `h264_backend="software"` and remove `nvenc_device`/CBR; the software
path does not probe the GPU. Do not delete a live worker record to force recovery.
Package install/removal and real hardware rollback are not tested here.
