//! Synchronous capture-thread facade over the async process supervisor.
//! No native driver runs here. A current-thread runtime services only IPC and
//! process deadlines while the Wayland capture thread waits for one frame.
use std::{marker::PhantomData, path::Path, rc::Rc};

use tokio::{process::Command, runtime::{Builder, Handle, Runtime}};

use super::{Config, Deadlines, DeviceLease, EncodedFrame, Error, Frame, Stamp, Worker};

pub struct BlockingWorker {
    worker: Option<Worker>,
    runtime: Option<Runtime>,
    stamp: Stamp,
    _capture_thread: PhantomData<Rc<()>>,
}

impl BlockingWorker {
    /// Called from a blocking capture thread, never a Tokio executor thread.
    /// As with Worker::spawn, a classified backend startup error is returned
    /// only after confirmed process exit. Failed reap returns Quarantined.
    pub fn spawn(command: Command, root: &Path, config: Config, deadlines: Deadlines) -> Result<Self, Error> {
        if Handle::try_current().is_ok() {
            return Err(Error::InvalidConfig);
        }
        config.frame_bytes()?;
        let runtime = Builder::new_current_thread().enable_all().build().map_err(|_| Error::Spawn)?;
        let lease = DeviceLease::acquire(root, config.device)?;
        let stamp = Stamp { epoch: config.epoch, sequence: 0 };
        let worker = runtime.block_on(Worker::spawn(command, lease, config, deadlines))?;
        Ok(Self { worker: Some(worker), runtime: Some(runtime), stamp, _capture_thread: PhantomData })
    }

    pub fn encode(&mut self, nv12: &[u8], force_idr: bool) -> Result<EncodedFrame, Error> {
        if Handle::try_current().is_ok() {
            return Err(Error::InvalidConfig);
        }
        let worker = self.worker.as_mut().ok_or(Error::Closed)?;
        let runtime = self.runtime.as_ref().ok_or(Error::Closed)?;
        let result = runtime.block_on(worker.encode(Frame { stamp: self.stamp, force_idr, nv12 }));
        if result.is_ok() {
            self.stamp.sequence = self.stamp.sequence.checked_add(1).ok_or(Error::InvalidFrame)?;
        }
        result
    }

    pub fn was_reaped(&self) -> bool {
        self.worker.as_ref().is_none_or(Worker::was_reaped)
    }

    pub fn shutdown(&mut self) -> Result<(), Error> {
        if Handle::try_current().is_ok() {
            return Err(Error::InvalidConfig);
        }
        let Some(mut worker) = self.worker.take() else { return Ok(()); };
        self.runtime.as_ref().ok_or(Error::Closed)?.block_on(worker.shutdown())
    }
}

impl Drop for BlockingWorker {
    fn drop(&mut self) {
        // Normal ownership is capture-thread confined and drains/reaps here.
        // If a caller violates the blocking-thread contract, never nest a
        // runtime or panic during Drop: Child kill_on_drop and its persistent
        // lease record keep the device closed to another worker until exit.
        if Handle::try_current().is_err() {
            let _ = self.shutdown();
        }
        self.worker.take();
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}
