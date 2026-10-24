//! NVENC lifecycle, private worker transport and failure policy.
//!
//! Native driver calls belong exclusively in the worker. No implementation of
//! [`Driver`] that accesses a GPU is supplied in this stage. See ADR 0001.

mod engine;
mod lease;
mod policy;
mod protocol;
mod supervisor;
mod types;

pub use engine::{Driver, Engine, Submission};
pub use lease::DeviceLease;
pub use policy::{plan, software_fallback_allowed, BackendMode, DeviceClass, Plan};
pub use supervisor::{serve, Deadlines, Worker, WORKER_FD};
pub use types::{AccessUnit, Config, DeviceId, EncodedFrame, Error, Fault, Frame, Phase, Stamp};

/// Allocated buffers are separate from the single accepted frame in flight.
pub const BUFFER_COUNT: usize = 4;
/// Hard IPC allocation limits, independent of any driver-provided dimensions.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

#[cfg(test)]
mod tests;
