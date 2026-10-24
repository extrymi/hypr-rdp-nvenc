//! Native ABI boundary. Called only in the private, supervised worker process.
mod driver;
mod loader;

// These bindings are generated from the pinned third-party C headers. Their
// naming, unused declarations and unsafe bitfield accessors are not handwritten.
#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    clippy::all
)]
mod ffi {
    include!(concat!(env!("OUT_DIR"), "/nvenc.rs"));
}

/// No library is loaded until the supervisor has persisted the worker identity
/// and sent Init. Existing serve() owns the CPU-bounded protocol and lifecycle.
pub fn serve(socket: std::os::unix::net::UnixStream) -> Result<(), super::Error> {
    if std::env::var("HYPR_RDP_NVENC_WORKER").as_deref() != Ok("1") {
        return Err(super::Error::Protocol);
    }
    super::serve(socket, driver::NativeDriver::unloaded())
}

#[cfg(test)]
mod tests;
