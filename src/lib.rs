//! Components shared by the RDP server and its private NVENC worker.
//! Native calls are optional and confined to the worker after supervised Init.

#[cfg(test)]
extern crate self as hypr_rdp;

#[cfg(target_os = "linux")]
pub mod nvenc;
