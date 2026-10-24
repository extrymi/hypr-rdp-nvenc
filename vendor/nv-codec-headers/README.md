# Vendored encoder interfaces

These two unmodified interface headers come from FFmpeg/nv-codec-headers,
released tag `n13.1.15.0`, commit `0a6fba9a2820628b8103464f4c8753ee05838baa`.
`source.json` records their original Git blobs and SHA-256 checksums. The full
permission/copyright notices are retained at the top of each header; both use
MIT terms applying to the header itself. This does not relicense NVIDIA's
proprietary driver, which is not bundled here.

Only interfaces are vendored. No FFmpeg, Sunshine or NVIDIA sample application
implementation is copied. Rust bindings and layout tests are generated from
these headers at build time, only when the `nvenc` feature is enabled.
No CUDA Toolkit, nvcc, CUDA runtime, GPU or installed NVIDIA headers are needed
to compile. Runtime loading uses the separately installed driver libraries.
