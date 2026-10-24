//! Components shared by the RDP server and its future NVENC worker.
//!
//! The NVENC module currently contains the CPU-testable lifecycle contract. It
//! does not load NVIDIA libraries or change the server's encoder selection.

#[cfg(test)]
extern crate self as hypr_rdp;

#[cfg(target_os = "linux")]
pub mod nvenc;
