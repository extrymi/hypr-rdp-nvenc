//! NVENC lifecycle, private worker transport and failure policy.
//!
//! Native driver calls belong exclusively in the worker. No implementation of
//! [`Driver`] is instantiated in the server. The optional `nvenc` feature adds
//! a lazy native adapter for worker use only. See ADR 0001 and native-nvenc.md.

mod blocking;
mod engine;
mod entry;
mod lease;
mod policy;
mod protocol;
mod supervisor;
mod types;

#[cfg(feature = "nvenc")]
mod annexb;
#[cfg(feature = "nvenc")]
pub mod native;

pub use blocking::BlockingWorker;
pub use engine::{Driver, Engine, Submission};
pub use entry::{dispatch_worker, WORKER_ARGUMENT};
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
